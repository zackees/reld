//! Derives the link TARGET (object format, architecture, class and endianness) purely from linker
//! argv tokens and the leading bytes of input files. This module does no I/O of its own: callers
//! are responsible for response-file expansion and for reading enough bytes of each input file.
//!
//! Precedence, earlier wins:
//! 1. `-m <emulation>` (separate token, or joined `-m<emulation>`; last one wins). Unknown
//!    emulation names are not a signal. The emulation fully determines format/arch/class/endian;
//!    `-EB`/`-EL` do NOT override it.
//! 2. `-arch <name>` (Mach-O; last one wins).
//! 3. Format-only argv signals: COFF `/OUT:<path>` / `-out:<path>` (case-insensitive prefix), or
//!    `-platform_version` followed by 3 tokens (Mach-O). If both appear, the COFF signal wins.
//!    arch/class/endian are then filled in from the first input whose header (see step 5) matches
//!    the resulting format; they stay `None` if no such input exists.
//! 4. `OUTPUT_FORMAT(...)` found in a text input (valid UTF-8, not starting with a binary magic).
//!    The one-arg form is used directly. The three-arg `(default, big, little)` form picks `big`
//!    if `-EB` was given, `little` if `-EL` was given, otherwise `default`. The first text input
//!    with a recognised `OUTPUT_FORMAT` wins.
//! 5. The first native object input header, in `inputs` order: ELF, Mach-O (thin or fat), or COFF
//!    (including the bigobj/short-import form). Archive members are not inspected.
//! 6. LLVM bitcode (raw or wrapper magic), but only if no native object input exists anywhere in
//!    `inputs`.
//!
//! After steps 2-6 (but *not* step 1), if `-EB` or `-EL` appears in `args` (last one wins), the
//! resulting `endian` is overridden with Big/Little. `-EB`/`-EL` alone, with no other signal,
//! yields `None`.
//!
//! This module is not yet wired into routing (reld#123 Phase 3, see reld#178): adding it does not
//! change the behaviour of any link.

#![cfg_attr(not(test), allow(dead_code))] // Wired into engine selection by the follow-up to reld#178 (reld#123 Phase 3).

use object::elf::DataEncoding;
use object::elf::ELFCLASS32;
use object::elf::ELFCLASS64;
use object::elf::ELFDATA2LSB;
use object::elf::ELFDATA2MSB;
use object::elf::ELFMAG;
use object::elf::EM_386;
use object::elf::EM_AARCH64;
use object::elf::EM_ARM;
use object::elf::EM_LOONGARCH;
use object::elf::EM_MIPS;
use object::elf::EM_PPC;
use object::elf::EM_PPC64;
use object::elf::EM_RISCV;
use object::elf::EM_S390;
use object::elf::EM_X86_64;
use object::elf::FileClass;
use object::elf::Machine;
use object::macho::CPU_TYPE_ARM;
use object::macho::CPU_TYPE_ARM64;
use object::macho::CPU_TYPE_ARM64_32;
use object::macho::CPU_TYPE_X86;
use object::macho::CPU_TYPE_X86_64;
use object::macho::CpuType;
use object::macho::FAT_MAGIC;
use object::macho::MH_MAGIC;
use object::macho::MH_MAGIC_64;
use std::fmt::Display;

// `object` has no `pe` feature in this workspace, so the COFF machine constants are defined here
// by hand, matching the values from the PE/COFF specification.
const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;
const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_FILE_MACHINE_ARM64: u16 = 0xaa64;
const IMAGE_FILE_MACHINE_ARMNT: u16 = 0x01c4;

/// Raw LLVM bitcode magic: `BC\xC0\xDE`.
const RAW_BITCODE_MAGIC: [u8; 4] = [0x42, 0x43, 0xC0, 0xDE];
/// LLVM bitcode wrapper magic, read little-endian.
const BITCODE_WRAPPER_MAGIC: u32 = 0x0B17_C0DE;
/// Bigobj / short-import COFF marker: the first two fields of the header are both zero/all-ones.
const COFF_BIGOBJ_PREFIX: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectFormat {
    Elf,
    Coff,
    MachO,
    LlvmBitcode,
}

impl Display for ObjectFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ObjectFormat::Elf => "ELF",
            ObjectFormat::Coff => "COFF",
            ObjectFormat::MachO => "Mach-O",
            ObjectFormat::LlvmBitcode => "LLVM bitcode",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbedArch {
    X86_64,
    I386,
    AArch64,
    Arm,
    RiscV32,
    RiscV64,
    LoongArch32,
    LoongArch64,
    PowerPc,
    PowerPc64,
    Mips,
    Mips64,
    S390,
    S390x,
}

impl Display for ProbedArch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProbedArch::X86_64 => "x86_64",
            ProbedArch::I386 => "i386",
            ProbedArch::AArch64 => "aarch64",
            ProbedArch::Arm => "arm",
            ProbedArch::RiscV32 => "riscv32",
            ProbedArch::RiscV64 => "riscv64",
            ProbedArch::LoongArch32 => "loongarch32",
            ProbedArch::LoongArch64 => "loongarch64",
            ProbedArch::PowerPc => "ppc",
            ProbedArch::PowerPc64 => "ppc64",
            ProbedArch::Mips => "mips",
            ProbedArch::Mips64 => "mips64",
            ProbedArch::S390 => "s390",
            ProbedArch::S390x => "s390x",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Bits32,
    Bits64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endian {
    Little,
    Big,
}

/// Which signal decided the object format (and, where it carries one, the architecture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    Emulation,
    MachOArch,
    CoffOut,
    MachOPlatformVersion,
    OutputFormat,
    InputHeader,
    Bitcode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbedTarget {
    pub(crate) format: ObjectFormat,
    pub(crate) arch: Option<ProbedArch>,
    pub(crate) class: Option<Class>,
    pub(crate) endian: Option<Endian>,
    pub(crate) decided_by: Signal,
}

/// `format`, `arch`, `class`, `endian` bundled together the way the individual signal probes
/// produce them, before we know which `Signal` will end up being attached to them.
type FormatTuple = (
    ObjectFormat,
    Option<ProbedArch>,
    Option<Class>,
    Option<Endian>,
);

fn format_tuple(
    format: ObjectFormat,
    arch: ProbedArch,
    class: Class,
    endian: Endian,
) -> FormatTuple {
    (format, Some(arch), Some(class), Some(endian))
}

/// `args` are the linker argv tokens AFTER the program name (already response-file expanded by
/// the caller). `inputs` are the leading bytes (at least the first 64 bytes, or the whole file for
/// text/linker scripts) of each input file in command-line order, including any `-T` script the
/// caller wants considered. Returns None when no signal identifies an object format.
pub(crate) fn probe_target<S: AsRef<str>>(args: &[S], inputs: &[&[u8]]) -> Option<ProbedTarget> {
    if let Some(target) = last_emulation_target(args) {
        return Some(target);
    }

    let endian_override = last_endian_override(args);

    if let Some(target) = last_macho_arch_target(args) {
        return Some(with_endian_override(target, endian_override));
    }

    if let Some(target) = format_only_signal_target(args, inputs) {
        return Some(with_endian_override(target, endian_override));
    }

    if let Some(target) = output_format_target(inputs, endian_override) {
        return Some(with_endian_override(target, endian_override));
    }

    if let Some(target) = first_native_input_target(inputs) {
        return Some(with_endian_override(target, endian_override));
    }

    if let Some(target) = first_bitcode_target(inputs) {
        return Some(with_endian_override(target, endian_override));
    }

    None
}

fn with_endian_override(mut target: ProbedTarget, endian: Option<Endian>) -> ProbedTarget {
    if endian.is_some() {
        target.endian = endian;
    }
    target
}

/// Returns the endianness requested by the last `-EB`/`-EL` in `args`, if any.
fn last_endian_override<S: AsRef<str>>(args: &[S]) -> Option<Endian> {
    let mut result = None;
    for arg in args {
        match arg.as_ref() {
            "-EB" => result = Some(Endian::Big),
            "-EL" => result = Some(Endian::Little),
            _ => {}
        }
    }
    result
}

// --- Step 1: `-m <emulation>` --------------------------------------------------------------

fn last_emulation_target<S: AsRef<str>>(args: &[S]) -> Option<ProbedTarget> {
    let mut found = None;

    for (index, arg) in args.iter().enumerate() {
        let arg = arg.as_ref();
        let emulation = if arg == "-m" {
            args.get(index + 1).map(|value| value.as_ref())
        } else {
            arg.strip_prefix("-m").filter(|rest| !rest.is_empty())
        };

        if let Some(tuple) = emulation.and_then(probe_emulation) {
            found = Some(tuple);
        }
    }

    found.map(|(format, arch, class, endian)| ProbedTarget {
        format,
        arch,
        class,
        endian,
        decided_by: Signal::Emulation,
    })
}

fn probe_emulation(name: &str) -> Option<FormatTuple> {
    use Class::Bits32;
    use Class::Bits64;
    use Endian::Big;
    use Endian::Little;
    use ObjectFormat::Coff;
    use ObjectFormat::Elf;
    use ProbedArch::AArch64;
    use ProbedArch::Arm;
    use ProbedArch::I386;
    use ProbedArch::LoongArch32;
    use ProbedArch::LoongArch64;
    use ProbedArch::Mips;
    use ProbedArch::Mips64;
    use ProbedArch::PowerPc;
    use ProbedArch::PowerPc64;
    use ProbedArch::RiscV32;
    use ProbedArch::RiscV64;
    use ProbedArch::S390;
    use ProbedArch::S390x;
    use ProbedArch::X86_64;

    let tuple = match name {
        "elf_x86_64" | "elf_x86_64_sol2" => format_tuple(Elf, X86_64, Bits64, Little),
        "elf32_x86_64" => format_tuple(Elf, X86_64, Bits32, Little),
        "elf_i386" => format_tuple(Elf, I386, Bits32, Little),
        "aarch64linux" | "aarch64elf" => format_tuple(Elf, AArch64, Bits64, Little),
        "aarch64linuxb" | "aarch64elfb" => format_tuple(Elf, AArch64, Bits64, Big),
        "armelf" | "armelf_linux_eabi" => format_tuple(Elf, Arm, Bits32, Little),
        "armelfb" | "armelfb_linux_eabi" => format_tuple(Elf, Arm, Bits32, Big),
        "elf32lriscv" => format_tuple(Elf, RiscV32, Bits32, Little),
        "elf64lriscv" => format_tuple(Elf, RiscV64, Bits64, Little),
        "elf32loongarch" => format_tuple(Elf, LoongArch32, Bits32, Little),
        "elf64loongarch" => format_tuple(Elf, LoongArch64, Bits64, Little),
        "elf32ppc" | "elf32ppclinux" => format_tuple(Elf, PowerPc, Bits32, Big),
        "elf32lppc" | "elf32lppclinux" => format_tuple(Elf, PowerPc, Bits32, Little),
        "elf64ppc" => format_tuple(Elf, PowerPc64, Bits64, Big),
        "elf64lppc" => format_tuple(Elf, PowerPc64, Bits64, Little),
        "elf32btsmip" | "elf32ebmip" => format_tuple(Elf, Mips, Bits32, Big),
        "elf32ltsmip" | "elf32elmip" => format_tuple(Elf, Mips, Bits32, Little),
        "elf32btsmipn32" => format_tuple(Elf, Mips64, Bits32, Big),
        "elf32ltsmipn32" => format_tuple(Elf, Mips64, Bits32, Little),
        "elf64btsmip" => format_tuple(Elf, Mips64, Bits64, Big),
        "elf64ltsmip" => format_tuple(Elf, Mips64, Bits64, Little),
        "elf_s390" => format_tuple(Elf, S390, Bits32, Big),
        "elf64_s390" => format_tuple(Elf, S390x, Bits64, Big),
        "i386pe" => format_tuple(Coff, I386, Bits32, Little),
        "i386pep" => format_tuple(Coff, X86_64, Bits64, Little),
        "arm64pe" => format_tuple(Coff, AArch64, Bits64, Little),
        "thumb2pe" => format_tuple(Coff, Arm, Bits32, Little),
        _ => return None,
    };

    Some(tuple)
}

// --- Step 2: `-arch <name>` -----------------------------------------------------------------

fn last_macho_arch_target<S: AsRef<str>>(args: &[S]) -> Option<ProbedTarget> {
    let mut found = None;

    for (index, arg) in args.iter().enumerate() {
        if arg.as_ref() != "-arch" {
            continue;
        }

        if let Some(tuple) = args
            .get(index + 1)
            .and_then(|value| probe_macho_arch(value.as_ref()))
        {
            found = Some(tuple);
        }
    }

    found.map(|(format, arch, class, endian)| ProbedTarget {
        format,
        arch,
        class,
        endian,
        decided_by: Signal::MachOArch,
    })
}

fn probe_macho_arch(name: &str) -> Option<FormatTuple> {
    use Class::Bits32;
    use Class::Bits64;
    use Endian::Little;
    use ObjectFormat::MachO;
    use ProbedArch::AArch64;
    use ProbedArch::Arm;
    use ProbedArch::I386;
    use ProbedArch::X86_64;

    let tuple = match name {
        "x86_64" | "x86_64h" => format_tuple(MachO, X86_64, Bits64, Little),
        "arm64" | "arm64e" => format_tuple(MachO, AArch64, Bits64, Little),
        "arm64_32" => format_tuple(MachO, AArch64, Bits32, Little),
        "i386" => format_tuple(MachO, I386, Bits32, Little),
        "armv7" | "armv7s" | "armv7k" => format_tuple(MachO, Arm, Bits32, Little),
        _ => return None,
    };

    Some(tuple)
}

// --- Step 3: format-only argv signals (COFF `/OUT:`/`-out:`, Mach-O `-platform_version`) ----

fn format_only_signal_target<S: AsRef<str>>(args: &[S], inputs: &[&[u8]]) -> Option<ProbedTarget> {
    let (format, decided_by) = if args.iter().any(|arg| is_coff_out_flag(arg.as_ref())) {
        (ObjectFormat::Coff, Signal::CoffOut)
    } else if has_platform_version_flag(args) {
        (ObjectFormat::MachO, Signal::MachOPlatformVersion)
    } else {
        return None;
    };

    let mut arch = None;
    let mut class = None;
    let mut endian = None;

    for input in inputs {
        let Some((input_format, input_arch, input_class, input_endian)) = probe_input_header(input)
        else {
            continue;
        };

        if input_format == format {
            arch = input_arch;
            class = input_class;
            endian = input_endian;
            break;
        }
    }

    Some(ProbedTarget {
        format,
        arch,
        class,
        endian,
        decided_by,
    })
}

fn is_coff_out_flag(arg: &str) -> bool {
    let Some(prefix) = arg.as_bytes().get(..5) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(b"/out:") || prefix.eq_ignore_ascii_case(b"-out:")
}

fn has_platform_version_flag<S: AsRef<str>>(args: &[S]) -> bool {
    for (index, arg) in args.iter().enumerate() {
        if arg.as_ref() == "-platform_version" && args.len() > index + 3 {
            return true;
        }
    }
    false
}

// --- Step 4: `OUTPUT_FORMAT(...)` in a text input -------------------------------------------

fn output_format_target(inputs: &[&[u8]], endian_pref: Option<Endian>) -> Option<ProbedTarget> {
    for input in inputs {
        if !is_probably_text(input) {
            continue;
        }

        if let Some((format, arch, class, endian)) = find_output_format(input, endian_pref) {
            return Some(ProbedTarget {
                format,
                arch,
                class,
                endian,
                decided_by: Signal::OutputFormat,
            });
        }
    }

    None
}

fn is_probably_text(bytes: &[u8]) -> bool {
    !starts_with_known_binary_magic(bytes) && std::str::from_utf8(bytes).is_ok()
}

fn starts_with_known_binary_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(&ELFMAG)
        || bytes.starts_with(&MH_MAGIC.to_le_bytes())
        || bytes.starts_with(&MH_MAGIC.to_be_bytes())
        || bytes.starts_with(&MH_MAGIC_64.to_le_bytes())
        || bytes.starts_with(&MH_MAGIC_64.to_be_bytes())
        || bytes.starts_with(&FAT_MAGIC.to_be_bytes())
        || bytes.starts_with(b"!<arch>\n")
        || bytes.starts_with(&RAW_BITCODE_MAGIC)
        || bytes.starts_with(&BITCODE_WRAPPER_MAGIC.to_le_bytes())
}

fn find_output_format(bytes: &[u8], endian_pref: Option<Endian>) -> Option<FormatTuple> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (_, after_keyword) = text.split_once("OUTPUT_FORMAT")?;
    let rest = after_keyword.trim_start().strip_prefix('(')?;
    let close = rest.find(')')?;
    let contents = rest.get(..close)?;

    let tokens: Vec<&str> = contents
        .split(',')
        .map(|token| token.trim().trim_matches('"'))
        .collect();

    let bfd_name = match tokens.as_slice() {
        [single] => *single,
        [default, big, little] => match endian_pref {
            Some(Endian::Big) => *big,
            Some(Endian::Little) => *little,
            None => *default,
        },
        _ => return None,
    };

    probe_bfd_name(bfd_name)
}

fn probe_bfd_name(name: &str) -> Option<FormatTuple> {
    use Class::Bits32;
    use Class::Bits64;
    use Endian::Big;
    use Endian::Little;
    use ObjectFormat::Coff;
    use ObjectFormat::Elf;
    use ProbedArch::AArch64;
    use ProbedArch::Arm;
    use ProbedArch::I386;
    use ProbedArch::LoongArch32;
    use ProbedArch::LoongArch64;
    use ProbedArch::Mips;
    use ProbedArch::Mips64;
    use ProbedArch::PowerPc;
    use ProbedArch::PowerPc64;
    use ProbedArch::RiscV32;
    use ProbedArch::RiscV64;
    use ProbedArch::S390;
    use ProbedArch::S390x;
    use ProbedArch::X86_64;

    let tuple = match name {
        "elf64-x86-64" => format_tuple(Elf, X86_64, Bits64, Little),
        "elf32-x86-64" => format_tuple(Elf, X86_64, Bits32, Little),
        "elf32-i386" => format_tuple(Elf, I386, Bits32, Little),
        "elf64-littleaarch64" => format_tuple(Elf, AArch64, Bits64, Little),
        "elf64-bigaarch64" => format_tuple(Elf, AArch64, Bits64, Big),
        "elf32-littlearm" => format_tuple(Elf, Arm, Bits32, Little),
        "elf32-bigarm" => format_tuple(Elf, Arm, Bits32, Big),
        "elf32-littleriscv" => format_tuple(Elf, RiscV32, Bits32, Little),
        "elf64-littleriscv" => format_tuple(Elf, RiscV64, Bits64, Little),
        "elf32-loongarch" => format_tuple(Elf, LoongArch32, Bits32, Little),
        "elf64-loongarch" => format_tuple(Elf, LoongArch64, Bits64, Little),
        "elf32-powerpc" => format_tuple(Elf, PowerPc, Bits32, Big),
        "elf32-powerpcle" => format_tuple(Elf, PowerPc, Bits32, Little),
        "elf64-powerpc" => format_tuple(Elf, PowerPc64, Bits64, Big),
        "elf64-powerpcle" => format_tuple(Elf, PowerPc64, Bits64, Little),
        "elf32-tradbigmips" | "elf32-bigmips" => format_tuple(Elf, Mips, Bits32, Big),
        "elf32-tradlittlemips" | "elf32-littlemips" => format_tuple(Elf, Mips, Bits32, Little),
        "elf32-ntradbigmips" => format_tuple(Elf, Mips64, Bits32, Big),
        "elf32-ntradlittlemips" => format_tuple(Elf, Mips64, Bits32, Little),
        "elf64-tradbigmips" => format_tuple(Elf, Mips64, Bits64, Big),
        "elf64-tradlittlemips" => format_tuple(Elf, Mips64, Bits64, Little),
        "elf32-s390" => format_tuple(Elf, S390, Bits32, Big),
        "elf64-s390" => format_tuple(Elf, S390x, Bits64, Big),
        "pe-i386" => format_tuple(Coff, I386, Bits32, Little),
        "pei-x86-64" | "pe-x86-64" => format_tuple(Coff, X86_64, Bits64, Little),
        "pei-aarch64-little" | "pe-aarch64-little" => format_tuple(Coff, AArch64, Bits64, Little),
        _ => return None,
    };

    Some(tuple)
}

// --- Step 5: the first native object input header -------------------------------------------

fn first_native_input_target(inputs: &[&[u8]]) -> Option<ProbedTarget> {
    for input in inputs {
        if let Some((format, arch, class, endian)) = probe_input_header(input) {
            return Some(ProbedTarget {
                format,
                arch,
                class,
                endian,
                decided_by: Signal::InputHeader,
            });
        }
    }

    None
}

/// Tries to identify `bytes` as an ELF, Mach-O (thin or fat) or COFF object header. Archive
/// members are not inspected: an archive input is not a signal at this step.
fn probe_input_header(bytes: &[u8]) -> Option<FormatTuple> {
    probe_elf_header(bytes)
        .or_else(|| probe_macho_thin_header(bytes))
        .or_else(|| probe_coff_header(bytes))
}

fn probe_elf_header(bytes: &[u8]) -> Option<FormatTuple> {
    if bytes.len() < 20 || !bytes.starts_with(&ELFMAG) {
        return None;
    }

    let class = match bytes.get(4).map(|&byte| FileClass(byte)) {
        Some(ELFCLASS32) => Some(Class::Bits32),
        Some(ELFCLASS64) => Some(Class::Bits64),
        _ => None,
    };

    let endian = match bytes.get(5).map(|&byte| DataEncoding(byte)) {
        Some(ELFDATA2LSB) => Some(Endian::Little),
        Some(ELFDATA2MSB) => Some(Endian::Big),
        _ => None,
    };

    let raw_machine = match endian {
        Some(Endian::Little) => read_u16_le(bytes, 18),
        Some(Endian::Big) => read_u16_be(bytes, 18),
        None => None,
    };

    let arch = raw_machine.and_then(|raw| elf_machine_to_arch(raw, class));

    Some((ObjectFormat::Elf, arch, class, endian))
}

fn elf_machine_to_arch(raw_machine: u16, class: Option<Class>) -> Option<ProbedArch> {
    match Machine(raw_machine) {
        EM_X86_64 => Some(ProbedArch::X86_64),
        EM_386 => Some(ProbedArch::I386),
        EM_AARCH64 => Some(ProbedArch::AArch64),
        EM_ARM => Some(ProbedArch::Arm),
        EM_RISCV => match class {
            Some(Class::Bits32) => Some(ProbedArch::RiscV32),
            Some(Class::Bits64) => Some(ProbedArch::RiscV64),
            None => None,
        },
        EM_LOONGARCH => match class {
            Some(Class::Bits32) => Some(ProbedArch::LoongArch32),
            Some(Class::Bits64) => Some(ProbedArch::LoongArch64),
            None => None,
        },
        EM_PPC => Some(ProbedArch::PowerPc),
        EM_PPC64 => Some(ProbedArch::PowerPc64),
        EM_MIPS => Some(if class == Some(Class::Bits32) {
            ProbedArch::Mips
        } else {
            ProbedArch::Mips64
        }),
        EM_S390 => Some(if class == Some(Class::Bits32) {
            ProbedArch::S390
        } else {
            ProbedArch::S390x
        }),
        _ => None,
    }
}

fn probe_macho_thin_header(bytes: &[u8]) -> Option<FormatTuple> {
    let magic_le = read_u32_le(bytes, 0)?;
    let magic_be = read_u32_be(bytes, 0)?;

    let (endian, magic_class) = if magic_le == MH_MAGIC_64 {
        (Endian::Little, Class::Bits64)
    } else if magic_le == MH_MAGIC {
        (Endian::Little, Class::Bits32)
    } else if magic_be == MH_MAGIC_64 {
        (Endian::Big, Class::Bits64)
    } else if magic_be == MH_MAGIC {
        (Endian::Big, Class::Bits32)
    } else if magic_be == FAT_MAGIC {
        return Some((ObjectFormat::MachO, None, None, None));
    } else {
        return None;
    };

    let raw_cputype = match endian {
        Endian::Little => read_u32_le(bytes, 4),
        Endian::Big => read_u32_be(bytes, 4),
    };

    let cputype = raw_cputype.map(CpuType);
    let (arch, class) = match cputype.and_then(macho_cputype_arch_and_class) {
        Some((arch, class)) => (Some(arch), class),
        None => (None, magic_class),
    };

    Some((ObjectFormat::MachO, arch, Some(class), Some(endian)))
}

/// Maps a Mach-O `cputype` to its architecture and the class implied by that specific cputype
/// (not by whatever magic number it was found under). Used both for thin Mach-O headers and for
/// the bitcode wrapper, which carries a cputype but no magic-derived class of its own.
fn macho_cputype_arch_and_class(cputype: CpuType) -> Option<(ProbedArch, Class)> {
    match cputype {
        CPU_TYPE_X86_64 => Some((ProbedArch::X86_64, Class::Bits64)),
        CPU_TYPE_ARM64 => Some((ProbedArch::AArch64, Class::Bits64)),
        CPU_TYPE_ARM64_32 => Some((ProbedArch::AArch64, Class::Bits32)),
        CPU_TYPE_X86 => Some((ProbedArch::I386, Class::Bits32)),
        CPU_TYPE_ARM => Some((ProbedArch::Arm, Class::Bits32)),
        _ => None,
    }
}

fn probe_coff_header(bytes: &[u8]) -> Option<FormatTuple> {
    // Bigobj and short-import headers carry the machine at offset 6; a regular COFF header
    // carries it at offset 0 (and can never start with the bigobj prefix, whose machine would
    // read as IMAGE_FILE_MACHINE_UNKNOWN).
    if bytes.starts_with(&COFF_BIGOBJ_PREFIX) {
        return read_u16_le(bytes, 6).and_then(coff_machine_to_tuple);
    }

    if bytes.len() < 20 {
        return None;
    }

    read_u16_le(bytes, 0).and_then(coff_machine_to_tuple)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(raw))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(raw))
}

fn coff_machine_to_tuple(machine: u16) -> Option<FormatTuple> {
    use Class::Bits32;
    use Class::Bits64;
    use Endian::Little;
    use ObjectFormat::Coff;
    use ProbedArch::AArch64;
    use ProbedArch::Arm;
    use ProbedArch::I386;
    use ProbedArch::X86_64;

    let tuple = match machine {
        IMAGE_FILE_MACHINE_AMD64 => format_tuple(Coff, X86_64, Bits64, Little),
        IMAGE_FILE_MACHINE_I386 => format_tuple(Coff, I386, Bits32, Little),
        IMAGE_FILE_MACHINE_ARM64 => format_tuple(Coff, AArch64, Bits64, Little),
        IMAGE_FILE_MACHINE_ARMNT => format_tuple(Coff, Arm, Bits32, Little),
        _ => return None,
    };

    Some(tuple)
}

// --- Step 6: LLVM bitcode, only if no native object input exists ----------------------------

fn first_bitcode_target(inputs: &[&[u8]]) -> Option<ProbedTarget> {
    for input in inputs {
        if let Some((format, arch, class, endian)) = probe_bitcode(input) {
            return Some(ProbedTarget {
                format,
                arch,
                class,
                endian,
                decided_by: Signal::Bitcode,
            });
        }
    }

    None
}

fn probe_bitcode(bytes: &[u8]) -> Option<FormatTuple> {
    if bytes.starts_with(&RAW_BITCODE_MAGIC) {
        return Some((ObjectFormat::LlvmBitcode, None, None, None));
    }

    if read_u32_le(bytes, 0)? != BITCODE_WRAPPER_MAGIC {
        return None;
    }

    // The wrapper header is five little-endian u32s: magic, version, offset, size, cputype.
    let cputype = read_u32_le(bytes, 16).map(CpuType);
    let arch_and_class = cputype.and_then(macho_cputype_arch_and_class);
    let arch = arch_and_class.map(|(arch, _)| arch);
    let class = arch_and_class.map(|(_, class)| class);

    Some((ObjectFormat::LlvmBitcode, arch, class, Some(Endian::Little)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GNU ld's three-argument `OUTPUT_FORMAT(default, big, little)` form, as found in the MIPS
    /// default linker scripts.
    const MIPS_TRIPLE_SCRIPT: &str = concat!(
        "OUTPUT_FORMAT(\"elf32-tradbigmips\", ",
        "\"elf32-tradbigmips\", \"elf32-tradlittlemips\")\n",
    );

    fn elf_header(class: u8, data: u8, machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&ELFMAG);
        bytes[4] = class;
        bytes[5] = data;
        bytes[6] = 1; // EI_VERSION
        // bytes[7..16] stay zero: EI_OSABI, EI_ABIVERSION, EI_PAD.
        let (e_type, e_machine) = if data == ELFDATA2MSB.0 {
            (1_u16.to_be_bytes(), machine.to_be_bytes())
        } else {
            (1_u16.to_le_bytes(), machine.to_le_bytes())
        };
        bytes[16..18].copy_from_slice(&e_type);
        bytes[18..20].copy_from_slice(&e_machine);
        bytes
    }

    fn macho_header(magic_le: bool, cputype: u32) -> Vec<u8> {
        let is_64 = cputype & 0x0100_0000 != 0;
        let magic = if is_64 { MH_MAGIC_64 } else { MH_MAGIC };
        let magic_bytes = if magic_le {
            magic.to_le_bytes()
        } else {
            magic.to_be_bytes()
        };
        let cputype_bytes = if magic_le {
            cputype.to_le_bytes()
        } else {
            cputype.to_be_bytes()
        };
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&magic_bytes);
        bytes[4..8].copy_from_slice(&cputype_bytes);
        bytes
    }

    fn coff_header(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..2].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    struct Case {
        label: &'static str,
        args: Vec<&'static str>,
        inputs: Vec<Vec<u8>>,
        expected: Option<ProbedTarget>,
    }

    fn run_cases(cases: &[Case]) {
        for case in cases {
            let input_refs: Vec<&[u8]> = case.inputs.iter().map(Vec::as_slice).collect();
            assert_eq!(
                probe_target(&case.args, &input_refs),
                case.expected,
                "case {}",
                case.label
            );
        }
    }

    #[test]
    fn target_probe_picks_engine_before_format() {
        let wrapper_bitcode = {
            let mut bytes = vec![0xDE, 0xC0, 0x17, 0x0B];
            bytes.extend_from_slice(&[0_u8; 12]);
            bytes.extend_from_slice(&CPU_TYPE_ARM64.0.to_le_bytes());
            bytes
        };

        let cases = [
            Case {
                label: "-m i386pep with no inputs",
                args: vec!["-m", "i386pep"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Coff,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
            Case {
                label: "-m elf_i386",
                args: vec!["-m", "elf_i386"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::I386),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
            Case {
                label: "joined -melf_i386",
                args: vec!["-melf_i386"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::I386),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
            Case {
                label: "-arch arm64",
                args: vec!["-arch", "arm64"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::MachO,
                    arch: Some(ProbedArch::AArch64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::MachOArch,
                }),
            },
            Case {
                label: "/OUT:foo.exe with an AMD64 COFF input",
                args: vec!["/OUT:foo.exe"],
                inputs: vec![coff_header(IMAGE_FILE_MACHINE_AMD64)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Coff,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::CoffOut,
                }),
            },
            Case {
                label: "-out:foo.exe with no inputs",
                args: vec!["-out:foo.exe"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Coff,
                    arch: None,
                    class: None,
                    endian: None,
                    decided_by: Signal::CoffOut,
                }),
            },
            Case {
                label: "EM_ARM ELFCLASS32 LSB input only",
                args: vec![],
                inputs: vec![elf_header(ELFCLASS32.0, ELFDATA2LSB.0, EM_ARM.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::Arm),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "big-endian EM_PPC64 ELFCLASS64 input",
                args: vec![],
                inputs: vec![elf_header(ELFCLASS64.0, ELFDATA2MSB.0, EM_PPC64.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::PowerPc64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Big),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "EM_MIPS big-endian 32-bit input",
                args: vec![],
                inputs: vec![elf_header(ELFCLASS32.0, ELFDATA2MSB.0, EM_MIPS.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::Mips),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Big),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "raw LLVM bitcode",
                args: vec![],
                inputs: vec![vec![0x42, 0x43, 0xC0, 0xDE, 0, 0, 0, 0]],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::LlvmBitcode,
                    arch: None,
                    class: None,
                    endian: None,
                    decided_by: Signal::Bitcode,
                }),
            },
            Case {
                label: "wrapper bitcode with an arm64 cputype",
                args: vec![],
                inputs: vec![wrapper_bitcode],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::LlvmBitcode,
                    arch: Some(ProbedArch::AArch64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Bitcode,
                }),
            },
            Case {
                label: "little-endian 64-bit Mach-O x86_64 input",
                args: vec![],
                inputs: vec![macho_header(true, CPU_TYPE_X86_64.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::MachO,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "-platform_version with an arm64 Mach-O input",
                args: vec!["-platform_version", "macos", "11.0", "14.0"],
                inputs: vec![macho_header(true, CPU_TYPE_ARM64.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::MachO,
                    arch: Some(ProbedArch::AArch64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::MachOPlatformVersion,
                }),
            },
            Case {
                label: "OUTPUT_FORMAT mips triple with -EL",
                args: vec!["-EL"],
                inputs: vec![MIPS_TRIPLE_SCRIPT.as_bytes().to_vec()],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::Mips),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::OutputFormat,
                }),
            },
        ];

        run_cases(&cases);
    }

    #[test]
    fn target_probe_explicit_emulation_beats_inputs() {
        let cases = [
            Case {
                label: "-m elf_i386 beats a conflicting EM_X86_64 ELFCLASS64 input",
                args: vec!["-m", "elf_i386"],
                inputs: vec![elf_header(ELFCLASS64.0, ELFDATA2LSB.0, EM_X86_64.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::I386),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
            Case {
                label: "-m elf32ltsmip -EB does not override the emulation's endianness",
                args: vec!["-m", "elf32ltsmip", "-EB"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::Mips),
                    class: Some(Class::Bits32),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
            Case {
                label: "-m elf_i386 -m elf_x86_64: last -m wins",
                args: vec!["-m", "elf_i386", "-m", "elf_x86_64"],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::Emulation,
                }),
            },
        ];

        run_cases(&cases);
    }

    #[test]
    fn target_probe_precedence_between_inferred_signals() {
        let cases = [
            Case {
                label: "OUTPUT_FORMAT text input beats an EM_AARCH64 input",
                args: vec![],
                inputs: vec![
                    b"OUTPUT_FORMAT(elf64-x86-64)\n".to_vec(),
                    elf_header(ELFCLASS64.0, ELFDATA2LSB.0, EM_AARCH64.0),
                ],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::OutputFormat,
                }),
            },
            Case {
                label: "native ELF input wins over bitcode regardless of order",
                args: vec![],
                inputs: vec![
                    vec![0x42, 0x43, 0xC0, 0xDE, 0, 0, 0, 0],
                    elf_header(ELFCLASS64.0, ELFDATA2LSB.0, EM_AARCH64.0),
                ],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::AArch64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "-EB overrides the endianness of an InputHeader signal",
                args: vec!["-EB"],
                inputs: vec![elf_header(ELFCLASS64.0, ELFDATA2LSB.0, EM_AARCH64.0)],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::Elf,
                    arch: Some(ProbedArch::AArch64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Big),
                    decided_by: Signal::InputHeader,
                }),
            },
            Case {
                label: "-arch beats -platform_version",
                args: vec![
                    "-arch",
                    "x86_64",
                    "-platform_version",
                    "macos",
                    "11.0",
                    "14.0",
                ],
                inputs: vec![],
                expected: Some(ProbedTarget {
                    format: ObjectFormat::MachO,
                    arch: Some(ProbedArch::X86_64),
                    class: Some(Class::Bits64),
                    endian: Some(Endian::Little),
                    decided_by: Signal::MachOArch,
                }),
            },
        ];

        run_cases(&cases);
    }

    #[test]
    fn target_probe_no_signal_is_none() {
        let cases = [
            Case {
                label: "empty args and inputs",
                args: vec![],
                inputs: vec![],
                expected: None,
            },
            Case {
                label: "only -EB",
                args: vec!["-EB"],
                inputs: vec![],
                expected: None,
            },
            Case {
                label: "unknown emulation",
                args: vec!["-m", "bogus_emulation"],
                inputs: vec![],
                expected: None,
            },
            Case {
                label: "unknown -arch",
                args: vec!["-arch", "bogus"],
                inputs: vec![],
                expected: None,
            },
            Case {
                label: "truncated ELF magic (3 bytes) is not a panic or a signal",
                args: vec![],
                inputs: vec![vec![0x7f, b'E', b'L']],
                expected: None,
            },
            Case {
                label: "archive input is not inspected",
                args: vec![],
                inputs: vec![b"!<arch>\n".to_vec()],
                expected: None,
            },
            Case {
                label: "plain text without OUTPUT_FORMAT",
                args: vec![],
                inputs: vec![b"hello world\n".to_vec()],
                expected: None,
            },
            Case {
                label: "truncated ELF header (10 bytes) does not panic",
                args: vec![],
                inputs: vec![vec![0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0]],
                expected: None,
            },
        ];

        run_cases(&cases);
    }
}
