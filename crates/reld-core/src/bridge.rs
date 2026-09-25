//! Polylinker routing and subprocess bridges (issue #17).
//!
//! `Args::new` reuses the ELF/GNU parser for `Args::Coff` because reld doesn't yet have a native
//! COFF backend. Rather than feeding MSVC-style link arguments (`/OUT:`, `/DEFAULTLIB:`, …) into
//! that parser, we bypass parsing entirely for COFF targets and delegate the raw argv straight
//! through to `lld-link` (the COFF driver built into `rust-lld`, which ships with every Rust
//! toolchain). See issue #17 for the full design rationale and issue #18 for this phase's scope.
//!
//! BR-3 (issue #29) generalizes this module to also bridge Mach-O links to `ld64.lld` (the
//! Mach-O driver in `rust-lld`), following the exact same discovery + flavor-prefix strategy,
//! selected by the `BridgeTarget` passed into `run_bridge`/`discover_linker`.
//!
//! BR-6 adds capability-based ELF routing. Ordinary ELF links stay in reld's native engine, while
//! requests that need a capability the native engine does not implement are delegated to the ELF
//! driver in `lld`.
//!
//! reld#184 adds a MinGW engine (`BridgeTarget::MinGw`, bridged to `ld.lld -m i386pep` -- lld's own
//! GNU-syntax PE/COFF driver) and wires `target_probe` (reld#178) into engine selection through
//! `resolve_link_target`. The link target is now resolved target-first: an explicit flavor (COFF
//! `-flavor link`, Mach-O `-flavor darwin`/`ld64`) wins outright with no probing at all; otherwise a
//! `--engine=`/`RELD_ENGINE` override picks the format from the named engine; otherwise the
//! best-effort target probe (argv signals, then input file headers) picks it; only when nothing
//! signals a format at all does the host's default apply. See `resolve_link_target` for the exact
//! precedence and `LinkTarget`/`ExplicitFormat` for the types that carry it through to
//! `select_route`.
//!
//! This module intentionally never falls back to the closed-source MSVC `link.exe` (or to Apple's
//! `ld64`) -- silently doing so would poison benchmark comparability and mask discovery bugs
//! (issue #17, decision B2).
//!
//! reld#192 (a reld#123 Phase 3 sub-issue) makes Mach-O routing target-keyed rather than
//! host-keyed: a Mach-O signal (`-arch`, `-platform_version`, a `--target=`/`-target` Apple
//! triple, or a Mach-O input header) routes to `ld64.lld` from any host, including a Linux one
//! cross-linking Mach-O. The same issue also decided the "darwin-cc" rule: reld does not
//! translate clang-driver arguments (`-Wl,...`, `-nodefaultlibs`, `-mmacosx-version-min=...`,
//! ...), which is what rustc's default Apple linker-flavor, `darwin-cc`, would otherwise hand
//! straight through. A Mach-O-routed argv carrying one of those flags fails loudly, before any
//! engine selection, naming the flag and pointing at rustc's `-Clinker-flavor=ld64.lld`; see
//! `reject_darwin_cc_argv` and `flag_table::DARWIN_CC_DRIVER_FLAGS`.

use crate::bail;
use crate::error::Context;
use crate::error::Result;
use crate::platforms::path::EXE_SUFFIX;
use crate::platforms::path::TOOLCHAIN_DYLIB_SEARCH;
use crate::target_probe::Signal;
use crate::target_probe::TargetFamily;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

/// Name of the environment variable that overrides linker discovery. If set, its value is used
/// verbatim as the path to the format-capable linker to bridge to.
pub const RELD_BRIDGE_LINKER_ENV: &str = "RELD_BRIDGE_LINKER";

/// Which object format the bridge should delegate links for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeTarget {
    /// Linux ELF, normally linked natively and bridged to `ld.lld` when capabilities require it.
    Elf,
    /// Windows PE/COFF, bridged to `lld-link`.
    Coff,
    /// macOS Mach-O, bridged to `ld64.lld`.
    MachO,
    /// Windows PE/COFF from a GNU-style (MinGW) command line, bridged to `ld.lld -m i386pep`
    /// (lld's MinGW driver).
    MinGw,
}

impl BridgeTarget {
    /// The human-readable format label used in error messages.
    fn format_label(self) -> &'static str {
        match self {
            BridgeTarget::Elf => "ELF",
            BridgeTarget::Coff => "COFF",
            BridgeTarget::MachO => "Mach-O",
            BridgeTarget::MinGw => "PE/COFF (MinGW)",
        }
    }
}

/// A bundled linker engine: its public name, object format, invocation, and capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    /// The engine name, as accepted by `--engine=<name>` / `RELD_ENGINE`.
    name: &'static str,
    /// The object format this engine links.
    format: BridgeTarget,
    /// The file name (without extension) of the concrete linker driver this engine bridges to.
    linker_basename: &'static str,
    /// The `-flavor` value to pass to the multi-flavor `rust-lld` dispatcher to select this
    /// engine's driver.
    rust_lld_flavor: &'static str,
    /// Whether this engine runs in-process rather than through the subprocess bridge.
    native: bool,
    /// Link configurations this engine can satisfy without silently dropping their semantics.
    capabilities: &'static [Capability],
}

/// Capabilities that affect engine selection rather than merely argument spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    NativeControl,
    Lto,
    Icf,
    DiscardAll,
    CortexA53Erratum,
    TextRelocs,
    ForeignArch,
    LinkerScriptInsert,
}

impl Capability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Capability::NativeControl => "reld-only validation or diagnostic output",
            Capability::Lto => "LTO",
            Capability::Icf => "identical code folding",
            Capability::DiscardAll => "discarding local symbols",
            Capability::CortexA53Erratum => "Cortex-A53 erratum 843419 fixups",
            Capability::TextRelocs => "text-relocation policy",
            Capability::ForeignArch => "architectures outside the native engine's set",
            Capability::LinkerScriptInsert => "linker-script INSERT commands",
        }
    }
}

const NATIVE_RELD_CAPABILITIES: &[Capability] = &[Capability::NativeControl];
const LLD_CAPABILITIES: &[Capability] = &[
    Capability::Lto,
    Capability::Icf,
    Capability::DiscardAll,
    Capability::CortexA53Erratum,
    Capability::TextRelocs,
    Capability::ForeignArch,
    Capability::LinkerScriptInsert,
];
// Deliberately its own list, not `LLD_CAPABILITIES`: lld's MinGW driver is `ld.lld`'s ELF
// capability set does not carry over 1:1 (e.g. no Cortex-A53 erratum fixups, no `-z text`
// policy), and routing-maintenance.md forbids copying another engine's capability list.
const MINGW_LLD_CAPABILITIES: &[Capability] = &[Capability::Lto, Capability::Icf];

const NATIVE_RELD_ENGINE: Engine = Engine {
    name: "reld",
    format: BridgeTarget::Elf,
    linker_basename: "",
    rust_lld_flavor: "",
    native: true,
    capabilities: NATIVE_RELD_CAPABILITIES,
};
const ELF_LLD_ENGINE: Engine = Engine {
    name: "lld",
    format: BridgeTarget::Elf,
    linker_basename: "ld.lld",
    rust_lld_flavor: "gnu",
    native: false,
    capabilities: LLD_CAPABILITIES,
};
const COFF_LLD_ENGINE: Engine = Engine {
    name: "lld-link",
    format: BridgeTarget::Coff,
    linker_basename: "lld-link",
    rust_lld_flavor: "link",
    native: false,
    capabilities: LLD_CAPABILITIES,
};
const MACHO_LLD_ENGINE: Engine = Engine {
    name: "ld64.lld",
    format: BridgeTarget::MachO,
    linker_basename: "ld64.lld",
    rust_lld_flavor: "darwin",
    native: false,
    capabilities: LLD_CAPABILITIES,
};
// `-flavor gnu` with `-m i386pep` (or another PE emulation) dispatches rust-lld's multi-flavor
// driver to lld's MinGW COFF driver, the same one `ld.lld` uses for a GNU-syntax PE/COFF link.
const MINGW_LLD_ENGINE: Engine = Engine {
    name: "lld-mingw",
    format: BridgeTarget::MinGw,
    linker_basename: "ld.lld",
    rust_lld_flavor: "gnu",
    native: false,
    capabilities: MINGW_LLD_CAPABILITIES,
};

/// The available engines and their capabilities. Ordering defines the default (fastest) engine
/// for each format: native reld for ELF, and the appropriate lld driver for COFF/Mach-O/MinGW.
const ENGINES: &[Engine] = &[
    NATIVE_RELD_ENGINE,
    ELF_LLD_ENGINE,
    COFF_LLD_ENGINE,
    MACHO_LLD_ENGINE,
    MINGW_LLD_ENGINE,
];

impl Engine {
    /// Looks up a bundled engine by name.
    fn find(name: &str) -> Option<&'static Engine> {
        ENGINES.iter().find(|engine| engine.name == name)
    }

    /// The default engine for a given target format (today's fixed platform->engine mapping).
    fn default_for(target: BridgeTarget) -> &'static Engine {
        match target {
            BridgeTarget::Elf => &NATIVE_RELD_ENGINE,
            BridgeTarget::Coff => &COFF_LLD_ENGINE,
            BridgeTarget::MachO => &MACHO_LLD_ENGINE,
            BridgeTarget::MinGw => &MINGW_LLD_ENGINE,
        }
    }

    fn supports(self, requirements: &[Requirement]) -> bool {
        requirements
            .iter()
            .all(|requirement| self.capabilities.contains(&requirement.capability))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Requirement {
    capability: Capability,
    trigger: &'static str,
}

/// Where an engine selection came from, for the observable routing note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionReason {
    Default,
    OverrideFlag,
    OverrideEnv,
    Capability(&'static str),
    Target(&'static str),
}

impl SelectionReason {
    fn label(self) -> &'static str {
        match self {
            SelectionReason::Default => "default",
            SelectionReason::OverrideFlag => "override(--engine)",
            SelectionReason::OverrideEnv => "override(RELD_ENGINE)",
            SelectionReason::Capability(trigger) => trigger,
            SelectionReason::Target(label) => label,
        }
    }
}

/// The `SelectionReason::Target` label for a format the probe picked (as opposed to a format
/// requested by an override or an explicit flavor), keyed only by the resolved `BridgeTarget` so
/// the caller of `resolve_link_target` never has to know the label spellings.
fn target_selection_label(format: BridgeTarget) -> &'static str {
    match format {
        BridgeTarget::Elf => "target:elf",
        BridgeTarget::Coff => "target:coff",
        BridgeTarget::MachO => "target:mach-o",
        BridgeTarget::MinGw => "target:pe-mingw",
    }
}

/// The reld-specific argv flag that explicitly selects a bridge engine, stripped from the
/// forwarded argv before it reaches the child linker.
const ENGINE_FLAG_PREFIX: &str = "--engine=";

/// The environment variable that explicitly selects a bridge engine, checked when no `--engine=`
/// argv token is present.
pub const RELD_ENGINE_ENV: &str = "RELD_ENGINE";

/// Enables one routing-decision line on stderr when present in the environment.
pub const RELD_LOG_ENGINE_ENV: &str = "RELD_LOG_ENGINE";

/// Appends one JSON object after every successful link when set to a file path.
///
/// This is an acceptance-test audit channel, deliberately separate from human-readable stderr
/// logging. A record is written only after the selected native or bridge engine returns success.
pub const RELD_INVOCATION_LOG_ENV: &str = "RELD_INVOCATION_LOG";

fn route_logging_enabled() -> bool {
    std::env::var_os(RELD_LOG_ENGINE_ENV).is_some()
}

/// The selected engine and the reason it was selected for one link request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    engine: &'static Engine,
    reason: SelectionReason,
    /// The `-m <emulation>` to inject for [`MINGW_LLD_ENGINE`] when the probe determined the
    /// MinGW emulation from something other than an explicit `-m` (e.g. a bare COFF input on a
    /// GNU-syntax line): `None` whenever argv already carries its own `-m i386pe*`.
    mingw_emulation: Option<&'static str>,
}

fn decode_response_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read linker response file `{}`", path.display()))?;
    if bytes.starts_with(&[0xff, 0xfe])
        || (bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes[1] == 0)
    {
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .skip(usize::from(bytes.starts_with(&[0xff, 0xfe])))
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16(&words)
            .with_context(|| format!("Invalid UTF-16 linker response file `{}`", path.display()));
    }
    String::from_utf8(bytes)
        .with_context(|| format!("Invalid UTF-8 linker response file `{}`", path.display()))
}

/// Splits the decoded contents of a COFF/Mach-O linker response file into arguments.
///
/// Only COFF/Mach-O invocation-log output extraction uses this splitter, because lld-link/
/// ld64.lld own their response grammar and encoding; ELF never comes here, it goes through the
/// one GNU tokenizer in `args/response_file.rs` (reld#179).
fn bridged_response_arguments(contents: &str) -> Result<Vec<OsString>> {
    let mut arguments = Vec::new();
    let mut argument = String::new();
    let mut quote = None;
    for character in contents.chars() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, character) if character.is_whitespace() => {
                if !argument.is_empty() {
                    arguments.push(OsString::from(std::mem::take(&mut argument)));
                }
            }
            (_, character) => argument.push(character),
        }
    }
    if let Some(quote) = quote {
        bail!("Unclosed `{quote}` in linker response file");
    }
    if !argument.is_empty() {
        arguments.push(OsString::from(argument));
    }
    Ok(arguments)
}

fn requested_output_in(
    argv: &[OsString],
    target: BridgeTarget,
    response_depth: usize,
) -> Result<Option<String>> {
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        let value = argument.to_string_lossy();
        if let Some(path) = value.strip_prefix('@') {
            if response_depth >= 16 {
                bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
            }
            // ELF and MinGW response files go through the one GNU tokenizer (reld#179); COFF/
            // Mach-O keep their own UTF-16-aware decoding and splitter
            // (`bridged_response_arguments`).
            let nested = if matches!(target, BridgeTarget::Elf | BridgeTarget::MinGw) {
                crate::args::read_args_from_file(Path::new(path))?
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            } else {
                bridged_response_arguments(&decode_response_file(Path::new(path))?)?
            };
            if let Some(output) = requested_output_in(&nested, target, response_depth + 1)? {
                return Ok(Some(output));
            }
            continue;
        }
        if value == "-o" {
            return Ok(arguments
                .next()
                .map(|output| output.to_string_lossy().into_owned()));
        }
        let lowercase = value.to_ascii_lowercase();
        if let Some(output) = lowercase
            .strip_prefix("/out:")
            .or_else(|| lowercase.strip_prefix("-out:"))
        {
            let prefix_length = value.len() - output.len();
            return Ok(Some(value[prefix_length..].to_owned()));
        }
    }
    Ok(None)
}

fn requested_output(argv: &[OsString], target: BridgeTarget) -> Result<Option<String>> {
    requested_output_in(&argv[1..], target, 0)
}

/// What argv\[0\] / `-flavor` said about the command-line syntax, before probing. This is a
/// syntax signal, not a format signal: `Gnu` still leaves the format itself to be resolved (see
/// `resolve_link_target`), because a GNU-syntax line can be either ELF or, with a PE emulation,
/// MinGW.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExplicitFormat {
    /// No `-flavor`/argv0 dispatch happened; nothing has been decided about the syntax yet.
    None,
    /// `-flavor gnu` (the default), or any argv0 that isn't one of the MSVC/Mach-O aliases.
    Gnu,
    /// `-flavor link`, or argv0 `reld-link`.
    Coff,
    /// `-flavor darwin`/`ld64`, or argv0 `ld64`.
    MachO,
}

/// The resolved link target: the object format to route for, plus (whenever it was computed) the
/// best-effort target probe that either decided that format or that `select_route` still needs to
/// check for a `Capability::ForeignArch` trigger, even when the format itself came from an
/// override or an explicit flavor rather than from the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkTarget {
    format: BridgeTarget,
    probe: Option<crate::target_probe::ProbedTarget>,
    /// Whether step 4 of `resolve_link_target`'s precedence (the probe's family) is what decided
    /// `format`, as opposed to an explicit flavor, an override, or falling through to the host
    /// default. Kept distinct from `probe_changed_format` below because a probed format that
    /// happens to agree with what the host default would have been is still not "the target
    /// changed the route" for `SelectionReason::Target` purposes.
    probe_chose_format: bool,
    /// Whether the probe's family (step 4) chose a format different from what falling through to
    /// step 5 would have chosen. `select_route` uses this instead of `probe_chose_format` to
    /// decide `SelectionReason::Target`, so that bridge.rs never has to know the host format.
    probe_changed_format: bool,
}

impl LinkTarget {
    /// The resolved object format to route this link for.
    #[must_use]
    pub fn format(self) -> BridgeTarget {
        self.format
    }
}

impl From<BridgeTarget> for LinkTarget {
    fn from(format: BridgeTarget) -> Self {
        LinkTarget {
            format,
            probe: None,
            probe_chose_format: false,
            probe_changed_format: false,
        }
    }
}

/// Resolves the link target for one invocation, in precedence order (earlier wins):
///
/// 1. An explicit flavor: `ExplicitFormat::Coff`/`MachO` decide the format outright, with no
///    probing at all (`-flavor link`/`darwin`/`ld64`, or the matching argv0 alias, is as
///    unambiguous as a format signal gets).
/// 2. Otherwise, the best-effort target probe (`probe_link_target`) runs.
/// 3. An engine override -- the first `--engine=<name>` token in `args`, else `env_engine` --
///    that names a known engine picks that engine's format. Under `ExplicitFormat::Gnu`, only an
///    override to a GNU-syntax format (`Elf`/`MinGw`) is honored; any other override's format is
///    left as `Elf`, and `select_engine` errors loudly on the resulting mismatch later. An unknown
///    override name does not affect the format here either; `select_engine` errors on it.
/// 4. Otherwise, the probe's family, if it identified one: `Elf` maps to `Elf`, `MinGwCoff` to
///    `MinGw`, `MsvcCoff` to `Coff`, `MachO` to `MachO`. Under `ExplicitFormat::Gnu`, only
///    `MinGwCoff` maps to `MinGw`; every other family maps to `Elf` -- GNU syntax plus a PE
///    emulation is MinGW, exactly what lld itself does with `-flavor gnu -m i386pep`.
/// 5. Otherwise, `ExplicitFormat::Gnu` resolves to `Elf`, and `ExplicitFormat::None` resolves to
///    `host`.
///
/// The probe, once computed, is kept in the returned `LinkTarget` regardless of which step ends
/// up deciding the format: `select_route` needs it to check for a `Capability::ForeignArch`
/// trigger even when an override or an explicit flavor picked the format outright.
pub(crate) fn resolve_link_target<S: AsRef<str>>(
    explicit: ExplicitFormat,
    args: &[S],
    env_engine: Option<&str>,
    host: BridgeTarget,
) -> LinkTarget {
    match explicit {
        ExplicitFormat::Coff => return BridgeTarget::Coff.into(),
        ExplicitFormat::MachO => return BridgeTarget::MachO.into(),
        ExplicitFormat::None | ExplicitFormat::Gnu => {}
    }

    let probe = probe_link_target(args);

    let fallback_format = match explicit {
        ExplicitFormat::Gnu => BridgeTarget::Elf,
        ExplicitFormat::None => host,
        ExplicitFormat::Coff | ExplicitFormat::MachO => unreachable!("handled above"),
    };

    let override_name = args
        .iter()
        .find_map(|arg| arg.as_ref().strip_prefix(ENGINE_FLAG_PREFIX))
        .or(env_engine);
    if let Some(engine) = override_name.and_then(Engine::find) {
        let format = if explicit == ExplicitFormat::Gnu
            && !matches!(engine.format, BridgeTarget::Elf | BridgeTarget::MinGw)
        {
            BridgeTarget::Elf
        } else {
            engine.format
        };
        return LinkTarget {
            format,
            probe,
            probe_chose_format: false,
            probe_changed_format: false,
        };
    }

    if let Some(family) = probe.and_then(|probe| probe.family()) {
        let format = match (explicit, family) {
            (ExplicitFormat::Gnu, TargetFamily::MinGwCoff) => BridgeTarget::MinGw,
            (ExplicitFormat::Gnu, _) => BridgeTarget::Elf,
            (_, TargetFamily::Elf) => BridgeTarget::Elf,
            (_, TargetFamily::MinGwCoff) => BridgeTarget::MinGw,
            (_, TargetFamily::MsvcCoff) => BridgeTarget::Coff,
            (_, TargetFamily::MachO) => BridgeTarget::MachO,
        };
        return LinkTarget {
            format,
            probe,
            probe_chose_format: true,
            probe_changed_format: format != fallback_format,
        };
    }

    LinkTarget {
        format: fallback_format,
        probe,
        probe_chose_format: false,
        probe_changed_format: false,
    }
}

/// How many leading bytes of a candidate probe input (or a positional/input-carried-capability
/// input, see `probe_input_requirement`) are read before deciding whether it looks like text.
const PROBE_PREFIX_LEN: usize = 64;

/// Cap on how much of a text-like candidate input (e.g. a linker script) is read once it's been
/// identified as text, so a huge script can't make routing itself slow.
const PROBE_MAX_TEXT_INPUT_BYTES: u64 = 1024 * 1024;

/// Options that consume one or more following tokens as their value(s), for `probe_link_target`'s
/// candidate-input scan: the named count of tokens after the option is skipped rather than being
/// considered a candidate input path. `-platform_version` (a platform name and two version
/// numbers) and `-sectcreate` (segment, section, file) are the only entries that skip more than
/// one token; `-weak-l<name>` is deliberately absent, since it's always joined, never separate.
const PROBE_SEPARATE_VALUE_OPTIONS: &[(&str, usize)] = &[
    ("-o", 1),
    ("--output", 1),
    ("-L", 1),
    ("-l", 1),
    ("-e", 1),
    ("--entry", 1),
    ("-u", 1),
    ("--undefined", 1),
    ("-y", 1),
    ("-z", 1),
    ("-m", 1),
    ("-R", 1),
    ("-h", 1),
    ("-soname", 1),
    ("--soname", 1),
    ("-rpath", 1),
    ("--rpath", 1),
    ("-rpath-link", 1),
    ("--rpath-link", 1),
    ("-dynamic-linker", 1),
    ("--dynamic-linker", 1),
    ("-plugin", 1),
    ("--plugin", 1),
    ("-plugin-opt", 1),
    ("--plugin-opt", 1),
    ("-Map", 1),
    ("--Map", 1),
    ("--version-script", 1),
    ("--dynamic-list", 1),
    ("--sysroot", 1),
    ("-init", 1),
    ("-fini", 1),
    ("--wrap", 1),
    ("--defsym", 1),
    ("--hash-style", 1),
    ("--image-base", 1),
    ("--out-implib", 1),
    ("--subsystem", 1),
    ("-mllvm", 1),
    ("--build-id", 1),
    ("-arch", 1),
    ("-syslibroot", 1),
    ("-install_name", 1),
    ("-platform_version", 3),
    ("-lto_library", 1),
    ("-target", 1),
    ("-framework", 1),
    ("-weak_framework", 1),
    ("-exported_symbols_list", 1),
    ("-unexported_symbols_list", 1),
    ("-order_file", 1),
    ("-map", 1),
    ("-dependency_info", 1),
    ("-object_path_lto", 1),
    ("-current_version", 1),
    ("-compatibility_version", 1),
    ("-headerpad", 1),
    ("-undefined", 1),
    ("-sectcreate", 3),
];

/// Best-effort probe of the link target from `args` (the linker argv tokens after the program
/// name and after any `-flavor X` pair). Never returns an error, and never opens a FIFO or a tty:
/// on any I/O error, or past the 16-level `@file` nesting depth, it keeps going without the
/// affected tokens rather than failing the link over what is only a routing hint.
///
/// Performance: clang and gcc always pass an explicit `-m <emulation>` for a native Linux link, so
/// the argv-only signals (`Signal::Emulation`, `MachOArch`, `DriverTarget`, `CoffOut`,
/// `MachOPlatformVersion`) resolve without opening a single input file -- the common case does
/// zero extra I/O.
pub(crate) fn probe_link_target<S: AsRef<str>>(
    args: &[S],
) -> Option<crate::target_probe::ProbedTarget> {
    let expanded = expand_probe_response_files(args, 0);

    if let Some(target) = crate::target_probe::probe_target(&expanded, &[])
        && matches!(
            target.decided_by,
            crate::target_probe::Signal::Emulation
                | crate::target_probe::Signal::MachOArch
                | crate::target_probe::Signal::DriverTarget
                | crate::target_probe::Signal::CoffOut
                | crate::target_probe::Signal::MachOPlatformVersion
        )
    {
        return Some(target);
    }

    let owned_inputs: Vec<Vec<u8>> = probe_candidate_paths(&expanded)
        .into_iter()
        .filter_map(|path| read_probe_input(Path::new(path)))
        .collect();
    let input_refs: Vec<&[u8]> = owned_inputs.iter().map(Vec::as_slice).collect();

    crate::target_probe::probe_target(&expanded, &input_refs)
}

/// Recursively expands `@file` tokens for `probe_link_target`, depth-limited to 16 like every
/// other response-file expansion in this module. Beyond that depth, or on any read/decode error,
/// this keeps going without that file's tokens rather than erroring: the probe is a routing hint,
/// not validation.
fn expand_probe_response_files<S: AsRef<str>>(args: &[S], depth: usize) -> Vec<String> {
    let mut expanded = Vec::with_capacity(args.len());
    for arg in args {
        let arg = arg.as_ref();
        if let Some(path) = arg.strip_prefix('@') {
            if depth < 16
                && let Some(tokens) = read_probe_response_file(Path::new(path))
            {
                expanded.extend(expand_probe_response_files(&tokens, depth + 1));
            }
            continue;
        }
        expanded.push(arg.to_owned());
    }
    expanded
}

/// Reads and decodes one `@file` for `expand_probe_response_files`: UTF-16 COFF/Mach-O-style
/// response files through the existing decoder and splitter, everything else through the one GNU
/// tokenizer. `None` on any I/O or decode error.
fn read_probe_response_file(path: &Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xff, 0xfe])
        || (bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes[1] == 0)
    {
        let contents = decode_response_file(path).ok()?;
        let tokens = bridged_response_arguments(&contents).ok()?;
        return Some(
            tokens
                .into_iter()
                .map(|token| token.to_string_lossy().into_owned())
                .collect(),
        );
    }
    crate::args::read_args_from_file(path).ok()
}

/// Gathers candidate input paths from `args`, in command-line order, for `probe_link_target`:
/// `-T`/`--script` script arguments (separate or joined/`=`-joined), and every other token that
/// doesn't start with `-` and wasn't consumed as a separate-value option's value (see
/// `PROBE_SEPARATE_VALUE_OPTIONS`).
fn probe_candidate_paths(args: &[String]) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();

        if arg == "-T" || arg == "--script" {
            if let Some(path) = args.get(index + 1) {
                paths.push(path.as_str());
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(path) = arg.strip_prefix("-T").filter(|rest| !rest.is_empty()) {
            paths.push(path);
            index += 1;
            continue;
        }
        if let Some(path) = arg
            .strip_prefix("--script=")
            .or_else(|| arg.strip_prefix("-script="))
        {
            paths.push(path);
            index += 1;
            continue;
        }
        if let Some(&(_, skip)) = PROBE_SEPARATE_VALUE_OPTIONS
            .iter()
            .find(|(name, _)| *name == arg)
        {
            index += 1 + skip;
            continue;
        }
        if !arg.starts_with('-') {
            paths.push(arg);
        }
        index += 1;
    }
    paths
}

/// Reads one candidate probe input file: up to the first [`PROBE_PREFIX_LEN`] bytes, or the whole
/// file capped at [`PROBE_MAX_TEXT_INPUT_BYTES`] once it looks like text (a linker script). Skips
/// anything that isn't a regular file -- a FIFO or a tty must never be opened for reading here --
/// and treats any I/O error as "not a signal" rather than a hard failure.
fn read_probe_input(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::io::Seek as _;

    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut prefix = vec![0_u8; PROBE_PREFIX_LEN];
    let read = file.read(&mut prefix).ok()?;
    prefix.truncate(read);

    if crate::target_probe::looks_like_text(&prefix) {
        file.rewind().ok()?;
        let mut contents = Vec::new();
        file.take(PROBE_MAX_TEXT_INPUT_BYTES)
            .read_to_end(&mut contents)
            .ok()?;
        return Some(contents);
    }

    Some(prefix)
}

fn append_json_string(encoded: &mut String, value: &str) {
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character <= '\u{1f}' => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let value = character as usize;
                encoded.push_str("\\u00");
                encoded.push(HEX[value >> 4] as char);
                encoded.push(HEX[value & 0x0f] as char);
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
}

fn encode_invocation_record(
    argv: &[OsString],
    route: Route,
    working_directory: &Path,
    output: Option<&str>,
) -> String {
    let mut encoded = String::from("{\"schema\":1,\"status\":\"success\",\"process_id\":");
    encoded.push_str(&std::process::id().to_string());
    encoded.push_str(",\"working_directory\":");
    append_json_string(&mut encoded, &working_directory.to_string_lossy());
    encoded.push_str(",\"engine\":");
    append_json_string(&mut encoded, route.engine.name);
    encoded.push_str(",\"route_kind\":");
    append_json_string(
        &mut encoded,
        if route.engine.native {
            "native"
        } else {
            "bridge"
        },
    );
    encoded.push_str(",\"reason\":");
    append_json_string(&mut encoded, route.reason.label());
    encoded.push_str(",\"output\":");
    if let Some(output) = output {
        append_json_string(&mut encoded, output);
    } else {
        encoded.push_str("null");
    }
    encoded.push_str(",\"arguments\":[");
    for (index, argument) in argv.iter().skip(1).enumerate() {
        if index != 0 {
            encoded.push(',');
        }
        append_json_string(&mut encoded, &argument.to_string_lossy());
    }
    encoded.push_str("]}\n");
    encoded
}

/// Records one successfully completed linker invocation when [`RELD_INVOCATION_LOG_ENV`] is set.
///
/// The JSONL record includes enough information for an external test to match the exact output
/// artifact and selected engine. Failure to append is an error: silently losing the audit record
/// would let an acceptance test pass without proving that reld handled the link.
pub fn log_successful_invocation(argv: &[OsString], route: Route) -> Result<()> {
    let Some(path) = std::env::var_os(RELD_INVOCATION_LOG_ENV) else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let working_directory = std::env::current_dir()
        .with_context(|| "Failed to determine the linker working directory".to_owned())?;
    let output = requested_output(argv, route.engine.format)?;
    let encoded = encode_invocation_record(argv, route, &working_directory, output.as_deref());
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open invocation log `{}`", path.display()))?;
    log.write_all(encoded.as_bytes())
        .with_context(|| format!("Failed to append invocation log `{}`", path.display()))?;
    Ok(())
}

impl Route {
    /// Whether the selected engine must be invoked through the subprocess bridge.
    #[must_use]
    pub fn is_bridge(self) -> bool {
        !self.engine.native
    }

    /// Emits the observable routing decision for a native link.
    pub fn log_native(self) {
        debug_assert!(self.engine.native);
        if !route_logging_enabled() {
            return;
        }
        eprintln!(
            "reld: engine={} (native, reason={})",
            self.engine.name,
            self.reason.label()
        );
    }
}

/// Selects the bridge engine to use for `target`, honoring an explicit override name if given.
///
/// - `override_name` is `None`: returns the target's default engine (today's behavior).
/// - `override_name` is `Some(name)`: looks the engine up by name. An unknown name is a hard
///   error listing the valid engine names; a known engine whose format doesn't match `target` is
///   a hard error naming both the engine and the requested format.
fn select_engine(
    target: BridgeTarget,
    override_name: Option<&str>,
    requirements: &[Requirement],
) -> Result<&'static Engine> {
    if let Some(name) = override_name {
        let Some(engine) = Engine::find(name) else {
            let valid = ENGINES
                .iter()
                .map(|engine| engine.name)
                .collect::<Vec<_>>()
                .join(", ");
            bail!("Unknown engine `{name}`. Valid engines: {valid}.");
        };

        if engine.format != target {
            bail!(
                "Engine `{name}` links {} but this link is for {}. Choose an engine that supports {}.",
                engine.format.format_label(),
                target.format_label(),
                target.format_label(),
            );
        }

        if let Some(requirement) = requirements
            .iter()
            .find(|requirement| !engine.capabilities.contains(&requirement.capability))
        {
            bail!(
                "Engine `{name}` does not support {} requested by `{}`.",
                requirement.capability.label(),
                requirement.trigger,
            );
        }

        return Ok(engine);
    }

    let default = Engine::default_for(target);
    if default.supports(requirements) {
        return Ok(default);
    }

    if let Some(engine) = ENGINES
        .iter()
        .find(|engine| engine.format == target && engine.supports(requirements))
    {
        return Ok(engine);
    }

    let Some(requirement) = requirements
        .iter()
        .find(|requirement| !default.capabilities.contains(&requirement.capability))
    else {
        bail!("Internal error: capability fallback failed without an unmet requirement");
    };
    bail!(
        "No bundled {} engine supports {} requested by `{}`.",
        target.format_label(),
        requirement.capability.label(),
        requirement.trigger,
    );
}

/// reld-only flags that force the native engine because they request an artifact or output
/// content that only reld produces (validation, layout/trace dumps, symbol info, GC-stats
/// diagnostics, SFrame experiments). These are `Capability::NativeControl`: forcing native and
/// conflicting loudly with any routed requirement, rather than silently dropping the request
/// (#123 D6 "NativeControl"; agents/docs/routing-maintenance.md "Is it reld-only?").
const NATIVE_CONTROL_RELD_FLAGS: &[&str] = &[
    "validate-output",
    "write-layout",
    "write-trace",
    "sym-info",
    "write-gc-stats",
    "verbose-gc-stats",
    "got-plt-syms",
    "reld-experimental-sframe",
    "discard-sframe",
];

/// Whether `arg` is `--name` or `--name=<value>` for exactly `name`. GNU flags are case-sensitive
/// (agents/docs/routing-maintenance.md: "Spellings are case-sensitive for GNU and ld64 flags"), so
/// this never lowercases either side.
fn matches_reld_only_flag(arg: &str, name: &str) -> bool {
    arg.strip_prefix("--").is_some_and(|rest| {
        rest == name
            || rest
                .strip_prefix(name)
                .is_some_and(|value| value.starts_with('='))
    })
}

/// Checks whether `path` carries a capability requirement by its content, for the positional-
/// argument and `-T`/`--script` branches of `collect_requested_capabilities`:
///
/// - An LLVM bitcode magic number -- `BC\xC0\xDE` for raw bitcode, or the bitcode-wrapper magic
///   `0x0B17C0DE` (little-endian on disk: `DE C0 17 0B`) used when bitcode is embedded with a
///   wrapper header -- needs `Capability::Lto`. An LTO object with no `-flto`/`-plugin-opt` on the
///   command line must still route to lld (#123 Phase 0 row `-plugin-opt=… + bitcode inputs`): the
///   native engine has no LTO implementation and would silently mislink it.
/// - A linker script containing an `INSERT` command needs `Capability::LinkerScriptInsert`: reld's
///   native linker-script support does not implement `INSERT` (reld#184).
///
/// Archive members are not probed (#123 Phase 3 TargetProbe covers only bare file arguments). Only
/// regular files are opened: a FIFO or a tty (e.g. `-o /dev/stdout`) must never be read here. Any
/// I/O error is treated as "no requirement" rather than an error: this is a best-effort routing
/// probe, not validation.
fn probe_input_requirement(path: &Path) -> Option<Requirement> {
    use std::io::Read as _;
    use std::io::Seek as _;

    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut prefix = vec![0_u8; PROBE_PREFIX_LEN];
    let read = file.read(&mut prefix).ok()?;
    prefix.truncate(read);

    if prefix.starts_with(b"BC\xC0\xDE") || prefix.starts_with(&[0xDE, 0xC0, 0x17, 0x0B]) {
        return Some(Requirement {
            capability: Capability::Lto,
            trigger: "input:bitcode",
        });
    }

    if crate::target_probe::looks_like_text(&prefix) {
        file.rewind().ok()?;
        let mut contents = Vec::new();
        file.take(PROBE_MAX_TEXT_INPUT_BYTES)
            .read_to_end(&mut contents)
            .ok()?;
        if crate::target_probe::script_uses_insert(&contents) {
            return Some(Requirement {
                capability: Capability::LinkerScriptInsert,
                trigger: "input:linker-script-insert",
            });
        }
    }

    None
}

fn collect_requested_capabilities(
    args: impl IntoIterator<Item = OsString>,
    requirements: &mut Vec<Requirement>,
    response_depth: usize,
) -> Result<()> {
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let Some(arg) = arg.to_str() else {
            continue;
        };
        if let Some(path) = arg.strip_prefix('@') {
            if response_depth >= 16 {
                bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
            }
            let nested = crate::args::read_args_from_file(Path::new(path))?
                .into_iter()
                .map(OsString::from);
            collect_requested_capabilities(nested, requirements, response_depth + 1)?;
            continue;
        }

        let requirement = if NATIVE_CONTROL_RELD_FLAGS
            .iter()
            .any(|name| matches_reld_only_flag(arg, name))
        {
            Some(Requirement {
                capability: Capability::NativeControl,
                trigger: "flag:reld-only-control",
            })
        } else if is_driver_lto_flag(arg) {
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:-flto",
            })
        } else if arg == "-plugin"
            || arg == "--plugin"
            || arg.starts_with("-plugin=")
            || arg.starts_with("--plugin=")
        {
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:--plugin",
            })
        } else if arg.eq_ignore_ascii_case("/ltcg")
            || arg.to_ascii_lowercase().starts_with("/ltcg:")
            || arg.eq_ignore_ascii_case("/gl")
            || arg.to_ascii_lowercase().starts_with("/gl:")
        {
            // MSVC spellings (`/LTCG`, `/GL`) are case-insensitive
            // (agents/docs/routing-maintenance.md: "case-insensitive for COFF").
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:/LTCG",
            })
        } else if arg == "--icf=all"
            || arg == "--icf=safe"
            || arg == "-icf=all"
            || arg == "-icf=safe"
        {
            Some(Requirement {
                capability: Capability::Icf,
                trigger: "flag:--icf",
            })
        } else if arg == "--discard-all"
            || arg == "-discard-all"
            // `-x` (lowercase) is `--discard-all`; `-X` (uppercase) is
            // `--discard-locals`, a native default, so it must NOT match.
            || arg == "-x"
        {
            Some(Requirement {
                capability: Capability::DiscardAll,
                trigger: "flag:--discard-all",
            })
        } else if arg == "-plugin-opt"
            || arg.starts_with("-plugin-opt=")
            || arg == "--plugin-opt"
            || arg.starts_with("--plugin-opt=")
        {
            // rustc `-C linker-plugin-lto` through clang emits `-plugin-opt=…`
            // with no `-plugin`/`-flto`; bitcode needs lld's real LTO.
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:-plugin-opt",
            })
        } else if arg == "--fix-cortex-a53-843419" || arg == "-fix-cortex-a53-843419" {
            Some(Requirement {
                capability: Capability::CortexA53Erratum,
                trigger: "flag:--fix-cortex-a53-843419",
            })
        } else if arg == "-ztext"
            || arg == "-z=text"
            || (arg == "-z"
                && args
                    .peek()
                    .is_some_and(|next| next.to_str() == Some("text")))
        {
            // `-z text` errors on DT_TEXTREL text relocations; the native engine
            // never errors on them (it just sets DT_TEXTREL), so delegate to lld.
            Some(Requirement {
                capability: Capability::TextRelocs,
                trigger: "flag:-z text",
            })
        } else if arg == "-T" || arg == "--script" {
            args.next()
                .and_then(|value| probe_input_requirement(Path::new(&value)))
        } else if let Some(path) = arg.strip_prefix("-T").filter(|rest| !rest.is_empty()) {
            probe_input_requirement(Path::new(path))
        } else if let Some(path) = arg
            .strip_prefix("--script=")
            .or_else(|| arg.strip_prefix("-script="))
        {
            probe_input_requirement(Path::new(path))
        } else if !arg.is_empty() && !arg.starts_with('-') {
            probe_input_requirement(Path::new(arg))
        } else {
            None
        };

        if let Some(requirement) = requirement
            && !requirements
                .iter()
                .any(|existing: &Requirement| existing.capability == requirement.capability)
        {
            requirements.push(requirement);
        }
    }

    Ok(())
}

fn requested_capabilities(argv: &[OsString]) -> Result<Vec<Requirement>> {
    let mut requirements = Vec::new();
    collect_requested_capabilities(argv.iter().skip(1).cloned(), &mut requirements, 0)?;
    Ok(requirements)
}

fn override_from_request(
    argv: &[OsString],
    env_override: Option<&str>,
) -> (Option<String>, SelectionReason) {
    match engine_override_from_argv(argv) {
        Some(name) => (Some(name), SelectionReason::OverrideFlag),
        None => match env_override {
            Some(name) => (Some(name.to_owned()), SelectionReason::OverrideEnv),
            None => (None, SelectionReason::Default),
        },
    }
}

/// The canonical spelling of `arg` if it is a clang-driver-only flag with no `ld64.lld` equivalent
/// (reld#192; see `flag_table::DARWIN_CC_DRIVER_FLAGS`), or `None`. A spelling ending in `=` or
/// `,` matches as a prefix (`-Wl,`, `-mmacosx-version-min=`, `--target=`, `-fuse-ld=`,
/// `--ld-path=`, ...); every other spelling matches `arg` exactly. Case-sensitive, like every
/// other GNU/clang-driver flag reld classifies (agents/docs/routing-maintenance.md).
fn darwin_cc_driver_flag(arg: &str) -> Option<&'static str> {
    crate::flag_table::DARWIN_CC_DRIVER_FLAGS
        .iter()
        .find(|&&spelling| {
            if spelling.ends_with('=') || spelling.ends_with(',') {
                arg.starts_with(spelling)
            } else {
                arg == spelling
            }
        })
        .copied()
}

/// Rejects `argv` (the full process argv, including `argv[0]`) if it carries a clang-driver flag
/// with no `ld64.lld` equivalent (reld#192, the "darwin-cc" rule): reld does not translate
/// clang-driver arguments, so a Mach-O link that received them -- typically because rustc's
/// default Apple linker-flavor, `darwin-cc`, invoked reld through clang rather than through
/// rustc's `ld64.lld` linker-flavor -- must fail loudly and name the fix, rather than either
/// silently mis-linking or letting `ld64.lld` itself reject the flag with an unhelpful error.
///
/// Skips `argv[0]` and a leading `-flavor X` pair (mirroring `drop_reld_dispatch_tokens`), and
/// expands `@file` tokens best-effort with `expand_probe_response_files`: this is validation, not
/// routing, but the same best-effort expansion is good enough to catch a driver flag hidden in a
/// response file, and erring on the side of forwarding an unexpanded token to `ld64.lld` (which
/// would then produce its own, less actionable error) is an acceptable fallback.
fn reject_darwin_cc_argv(argv: &[OsString]) -> Result<()> {
    let mut rest: Vec<OsString> = argv.iter().skip(1).cloned().collect();
    if rest
        .first()
        .is_some_and(|arg| arg.as_os_str() == OsStr::new("-flavor"))
    {
        let drop = rest.len().min(2);
        rest.drain(0..drop);
    }

    let strings: Vec<String> = rest
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    for flag in expand_probe_response_files(&strings, 0) {
        if darwin_cc_driver_flag(&flag).is_some() {
            bail!(
                "This Mach-O link received the clang-driver argument `{flag}`, which has no \
                 ld64 equivalent: reld does not translate clang-driver (rustc `darwin-cc` \
                 linker-flavor) arguments. If rustc is invoking reld, add \
                 `-Clinker-flavor=ld64.lld` (for example \
                 CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS=-Clinker-flavor=ld64.lld). If a \
                 C/C++ build is linking, run the link through clang with `--ld-path=reld`. See \
                 reld#192."
            );
        }
    }

    Ok(())
}

fn select_route_with_policy(
    argv: &[OsString],
    target: LinkTarget,
    env_override: Option<&str>,
    allow_unsupported_native: bool,
    native_control_env: bool,
) -> Result<Route> {
    if target.format == BridgeTarget::MachO {
        // The darwin-cc rule (reld#192): checked unconditionally, before any engine selection or
        // override, and with no `RELD_UNSUPPORTED=ignore` escape hatch -- `ld64.lld` would reject
        // these flags anyway, so there is nothing an override could usefully do about them.
        reject_darwin_cc_argv(argv)?;
    }

    // COFF, Mach-O and MinGW each have a single bundled engine today, so parsing their response
    // files cannot affect selection. More importantly, their response grammars and encodings
    // differ from ELF/GNU (MSVC commonly emits UTF-16). Leave them byte-for-byte for the
    // destination driver instead of eagerly feeding them through reld's UTF-8 GNU response parser.
    let mut requirements = if target.format == BridgeTarget::Elf {
        let mut requirements = requested_capabilities(argv)?;
        // A foreign architecture is a routing reason in its own right, independent of any flag or
        // input-carried capability the argv classifier finds; put it first so it's the reported
        // reason (reld#123 D6, reld#184).
        if let Some(trigger) = target.probe.and_then(|probe| probe.foreign_arch_trigger()) {
            requirements.insert(
                0,
                Requirement {
                    capability: Capability::ForeignArch,
                    trigger,
                },
            );
        }
        requirements
    } else {
        Vec::new()
    };
    if target.format == BridgeTarget::Elf
        && native_control_env
        && !requirements
            .iter()
            .any(|requirement| requirement.capability == Capability::NativeControl)
    {
        requirements.push(Requirement {
            capability: Capability::NativeControl,
            trigger: "environment:reld-only-control",
        });
    }
    let (override_name, override_reason) = override_from_request(argv, env_override);
    let checked_requirements =
        if allow_unsupported_native && override_name.as_deref() == Some(NATIVE_RELD_ENGINE.name) {
            &[][..]
        } else {
            requirements.as_slice()
        };
    let engine = select_engine(
        target.format,
        override_name.as_deref(),
        checked_requirements,
    )?;
    let default = Engine::default_for(target.format);
    let reason = if override_name.is_some() {
        override_reason
    } else if engine != default {
        let Some(requirement) = requirements.first() else {
            bail!("Internal error: non-default route selected without a capability requirement");
        };
        SelectionReason::Capability(requirement.trigger)
    } else if target.probe_changed_format {
        // `probe_changed_format` is only ever set alongside `probe_chose_format` (see
        // `resolve_link_target`'s step 4), so this is always true; asserted rather than merely
        // implied, so the two fields can never silently drift apart.
        debug_assert!(target.probe_chose_format);
        SelectionReason::Target(target_selection_label(target.format))
    } else {
        SelectionReason::Default
    };

    // Inject the MinGW emulation only when the probe didn't see one already spelled out on argv
    // (`Signal::Emulation`): that's the case where a GNU-syntax line has only COFF inputs and no
    // `-m i386pe*`, so `ld.lld` still needs telling which PE/COFF flavor to emit.
    let emulation_on_argv = target
        .probe
        .is_some_and(|p| p.decided_by == Signal::Emulation);
    let mingw_emulation = if *engine == MINGW_LLD_ENGINE && !emulation_on_argv {
        target.probe.and_then(|probe| probe.mingw_emulation())
    } else {
        None
    };

    Ok(Route {
        engine,
        reason,
        mingw_emulation,
    })
}

#[cfg(test)]
fn select_route_with_env(
    argv: &[OsString],
    target: LinkTarget,
    env_override: Option<&str>,
) -> Result<Route> {
    select_route_with_policy(argv, target, env_override, false, false)
}

/// Selects the engine for one raw linker invocation.
pub fn select_route(argv: &[OsString], target: LinkTarget) -> Result<Route> {
    let native_control_env = [
        crate::args::VALIDATE_ENV,
        crate::args::WRITE_LAYOUT_ENV,
        crate::args::WRITE_TRACE_ENV,
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    let allow_unsupported_native = std::env::var(crate::args::RELD_UNSUPPORTED_ENV)
        .ok()
        .as_deref()
        == Some("ignore");

    select_route_with_policy(
        argv,
        target,
        std::env::var(RELD_ENGINE_ENV).ok().as_deref(),
        allow_unsupported_native,
        native_control_env,
    )
}

/// Locates the linker binary that the bridge should delegate to for the given engine.
///
/// Precedence:
/// 1. `RELD_BRIDGE_LINKER` env var, used verbatim. Errors if the path doesn't exist.
/// 2. `rust-lld` next to the active toolchain (`rustc --print sysroot`).
/// 3. `gcc-ld/<basename>` under that same rustlib bin dir, then `<basename>` on `PATH`.
/// 4. Otherwise, a hard error naming `RELD_BRIDGE_LINKER` and what to install.
fn discover_linker(engine: &Engine) -> Result<PathBuf> {
    debug_assert!(!engine.native);
    if let Ok(value) = std::env::var(RELD_BRIDGE_LINKER_ENV) {
        let path = PathBuf::from(value);
        if !path.exists() {
            bail!(
                "{RELD_BRIDGE_LINKER_ENV} is set to `{}`, but that path does not exist",
                path.display()
            );
        }
        return Ok(path);
    }

    if let Some(path) = find_rust_lld() {
        return Ok(path);
    }

    if let Some(path) = find_concrete_linker(engine) {
        return Ok(path);
    }

    bail!("{}", not_found_message(engine));
}

/// The error message shown when no format-capable linker can be discovered. Factored out so a
/// test can assert it names `RELD_BRIDGE_LINKER` without having to defeat real toolchain
/// discovery.
fn not_found_message(engine: &Engine) -> String {
    format!(
        "Could not find a {}-capable linker to bridge to. Install a Rust toolchain (which \
         provides `rust-lld`), put `{}` on PATH, or set {RELD_BRIDGE_LINKER_ENV} to the \
         path of a linker to use.",
        engine.format.format_label(),
        engine.linker_basename,
    )
}

/// Attempts to find `rust-lld` next to the currently active toolchain.
fn find_rust_lld() -> Option<PathBuf> {
    let candidate = rustlib_bin_dir()?.join(format!("rust-lld{EXE_SUFFIX}"));
    candidate.exists().then_some(candidate)
}

/// Attempts to find the engine's concrete linker driver: first under the active toolchain's
/// `gcc-ld` directory, then on `PATH`.
fn find_concrete_linker(engine: &Engine) -> Option<PathBuf> {
    let file_name = format!("{}{EXE_SUFFIX}", engine.linker_basename);

    if let Some(bin) = rustlib_bin_dir() {
        let candidate = bin.join("gcc-ld").join(&file_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    find_on_path(&file_name)
}

/// The `lib/rustlib/<host-triple>/bin` directory of the active toolchain, discovered via `rustc
/// --print sysroot`. Off the hot path (discovery runs once per link), so shelling out is fine.
fn rustlib_bin_dir() -> Option<PathBuf> {
    let output = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let sysroot = String::from_utf8(output.stdout).ok()?;
    let sysroot = sysroot.trim();
    if sysroot.is_empty() {
        return None;
    }

    let host_triple = host_triple()?;

    Some(
        Path::new(sysroot)
            .join("lib")
            .join("rustlib")
            .join(host_triple)
            .join("bin"),
    )
}

/// The root of the Rust toolchain that ships `linker`, when `linker` lives in that toolchain's
/// `lib/rustlib/<host>/bin` directory (`rust-lld`) or its `gcc-ld` subdirectory.
fn toolchain_root_of(linker: &Path) -> Option<&Path> {
    fn parent_named<'a>(path: &'a Path, name: &str) -> Option<&'a Path> {
        path.file_name()
            .is_some_and(|file_name| file_name == name)
            .then_some(path)
            .and_then(Path::parent)
    }
    let mut bin = linker.parent()?;
    if let Some(parent) = parent_named(bin, "gcc-ld") {
        bin = parent;
    }
    let rustlib = parent_named(bin, "bin")?.parent()?;
    parent_named(parent_named(rustlib, "rustlib")?, "lib")
}

/// The dynamic-loader search-path entry the child linker needs when it is a toolchain's own
/// `rust-lld`, as `(variable, value)` with the toolchain's shared-library directory prepended.
///
/// Since Rust 1.98 the Apple toolchains link `rust-lld` against `@rpath/libLLVM.dylib` with an rpath
/// of `@loader_path/../lib`, i.e. `lib/rustlib/<host>/lib`, but the `rustc` component ships the
/// library only as `<toolchain>/lib/libLLVM.dylib` (rust-lang/rust#157205). The toolchain works
/// under rustup only because rustup's proxies prepend `<toolchain>/lib` to the loader search path of
/// every tool they launch; a linker spawned by reld is not launched by a proxy, so reld does the
/// same thing (reld#206). The entry is added only when the directory exists.
fn toolchain_dylib_search_env(
    linker: &Path,
    current: Option<OsString>,
) -> Option<(&'static str, OsString)> {
    let (variable, relative_dir) = TOOLCHAIN_DYLIB_SEARCH;
    let dir = toolchain_root_of(linker)?.join(relative_dir);
    if !dir.is_dir() {
        return None;
    }
    let mut entries = vec![dir];
    if let Some(current) = current.filter(|value| !value.is_empty()) {
        entries.extend(std::env::split_paths(&current));
    }
    let value = std::env::join_paths(entries).ok()?;
    Some((variable, value))
}

/// Searches `PATH` for an executable with the given file name.
fn find_on_path(file_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(file_name);
        candidate.is_file().then_some(candidate)
    })
}

/// Returns the host target triple, e.g. `x86_64-pc-windows-msvc`, by parsing the `host:` line of
/// `rustc -vV`. (A compile-time `TARGET` is deliberately not used: cargo only sets it in a build
/// script's own environment, never in this crate's, and a stray ambient `TARGET` from a
/// cross-compile wrapper could otherwise leak in and give the wrong triple.)
fn host_triple() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
}

/// Whether the discovered linker needs an explicit `-flavor <target>` prefix to select the right
/// driver. `rust-lld` is a multi-flavor dispatcher, so it needs telling; the concrete driver
/// (`lld-link`, `ld64.lld`) is already the right driver by name, so it doesn't.
fn needs_flavor_prefix(linker: &Path) -> bool {
    linker
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("rust-lld"))
}

/// Drops `argv[0]` from the full process argv, and also drops a leading `-flavor <name>` pair if
/// present: when reld is invoked via the `-flavor link`/`-flavor darwin` multi-call convention
/// (see `Args::new`), that selector has already been consumed by reld's own platform dispatch, so
/// forwarding it would leak a stray `-flavor` into the child linker (and, for `rust-lld`, collide
/// with the `-flavor <target>` that `child_command_line` prepends).
///
/// Also strips any `--engine=<name>` token, wherever it appears: that's a reld-specific flag
/// (see `select_engine`), not a linker flag, so it must never leak into the child linker's
/// command line.
fn drop_reld_dispatch_tokens<I: IntoIterator<Item = OsString>>(argv: I) -> Vec<OsString> {
    let mut rest: Vec<OsString> = argv.into_iter().skip(1).collect();
    if rest
        .first()
        .is_some_and(|arg| arg.as_os_str() == OsStr::new("-flavor"))
    {
        // Drop `-flavor` and its argument (or just `-flavor` if it was the trailing token).
        let drop = rest.len().min(2);
        rest.drain(0..drop);
    }
    rest.retain(|arg| {
        arg.to_str()
            .is_none_or(|s| !s.starts_with(ENGINE_FLAG_PREFIX))
    });
    rest
}

/// How a `--name` flag's value is spelled, for `strip_reld_only_flags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrippedValueKind {
    /// The flag never takes a value.
    None,
    /// `--flag=value` is a value; a bare `--flag` takes the following token as its value only
    /// when that token doesn't start with `-` (as today -- this can misidentify a real input
    /// file as the value, a pre-existing quirk this table does not change).
    Optional,
    /// The flag always has a value: `--flag=value`, or otherwise the very next token regardless
    /// of its own spelling.
    Required,
}

/// reld-only performance/diagnostic knobs that must not reach a child linker. A link that routes
/// to a bridge engine (lld) drops these from the forwarded argv rather than forwarding an option
/// lld would reject (reld#123 D6 "Stripped"). `--rpath-link` is deliberately absent: `ld.lld`
/// documents it as "Ignored for compatibility" (agents/docs/routing-maintenance.md), so it is
/// forwarded like any ordinary linker flag instead of being stripped.
const STRIPPED_RELD_FLAGS: &[(&str, StrippedValueKind)] = &[
    ("update-in-place", StrippedValueKind::None),
    ("no-update-in-place", StrippedValueKind::None),
    ("time", StrippedValueKind::Optional),
    ("mmap-output-file", StrippedValueKind::None),
    ("no-mmap-output-file", StrippedValueKind::None),
    ("fallocate-output-file", StrippedValueKind::None),
    ("no-fallocate-output-file", StrippedValueKind::None),
    ("madvise-huge-pages", StrippedValueKind::None),
    ("no-madvise-huge-pages", StrippedValueKind::None),
    ("threads", StrippedValueKind::Optional),
    ("no-threads", StrippedValueKind::None),
    ("thread-count", StrippedValueKind::Required),
    ("fork", StrippedValueKind::None),
    ("no-fork", StrippedValueKind::None),
    ("prepopulate-maps", StrippedValueKind::None),
    ("debug-fuel", StrippedValueKind::Required),
    ("reld-experiments", StrippedValueKind::Required),
    ("gc-stats-ignore", StrippedValueKind::Required),
    ("nix-rpath", StrippedValueKind::Required),
    ("no-identity-comment", StrippedValueKind::None),
    ("no-string-merge", StrippedValueKind::None),
];

/// Drops reld-only flags from `args` so nothing reld-only reaches a child linker. Returns the
/// filtered argv alongside every dropped option as spelled on the command line -- a split value
/// rejoined with one space (e.g. `--thread-count 4`) -- for `stripped_options_note`.
fn strip_reld_only_flags(args: Vec<OsString>) -> (Vec<OsString>, Vec<String>) {
    let mut out = Vec::new();
    let mut dropped = Vec::new();
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        let Some(s) = arg.to_str() else {
            out.push(arg);
            continue;
        };
        let Some(bare) = s.strip_prefix("--") else {
            out.push(arg);
            continue;
        };
        let name = bare.split('=').next().unwrap_or(bare);
        let Some(&(_, kind)) = STRIPPED_RELD_FLAGS.iter().find(|entry| entry.0 == name) else {
            out.push(arg);
            continue;
        };

        let mut spelling = s.to_owned();
        if !bare.contains('=') {
            let takes_following_value = match kind {
                StrippedValueKind::None => false,
                StrippedValueKind::Required => true,
                StrippedValueKind::Optional => iter
                    .peek()
                    .is_some_and(|next| !next.to_str().is_some_and(|v| v.starts_with('-'))),
            };
            if takes_following_value && let Some(value) = iter.next() {
                spelling.push(' ');
                spelling.push_str(&value.to_string_lossy());
            }
        }
        dropped.push(spelling);
    }
    (out, dropped)
}

/// Computes the arguments to forward to the child linker from the full process argv: the reld
/// dispatch tokens dropped (see `drop_reld_dispatch_tokens`), then every top-level reld-only flag
/// stripped (see `strip_reld_only_flags`).
#[cfg(test)]
fn forwarded_args<I: IntoIterator<Item = OsString>>(argv: I) -> Vec<OsString> {
    strip_reld_only_flags(drop_reld_dispatch_tokens(argv)).0
}

/// Builds the human-readable `RELD_LOG_ENGINE` note listing every reld-only flag dropped before
/// forwarding to `engine_name`, or `None` if nothing was dropped. Deliberately does not start
/// with `reld: engine=`: `ci/linker_modes.py` parses that exact prefix to find the
/// routing-decision line, and this note is a separate, optional line after it.
fn stripped_options_note(engine_name: &str, dropped: &[String]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    Some(format!(
        "reld: not forwarding reld-only options to {engine_name}: {}",
        dropped.join(", ")
    ))
}

/// Removes compiler-driver-only LTO switches and every reld-only flag before forwarding to a
/// bridge engine. Returns the forwarded argv alongside every flag dropped as reld-only (see
/// `strip_reld_only_flags`).
///
/// For a bridged GNU-syntax engine (ELF `lld`, or MinGW `lld-mingw`), `expand_and_strip_driver_lto`
/// runs first so GNU-tokenized `@response` files are expanded before reld-only flags are stripped
/// -- otherwise a reld-only flag hidden inside a response file would reach `ld.lld` unstripped.
/// LLD also consumes LLVM bitcode directly and rejects `-flto` itself; plugin switches are real
/// linker options and remain untouched. Other engines' response files stay opaque (COFF/Mach-O own
/// their response grammar and encoding; see `bridged_response_arguments`), so only their top-level
/// argv is stripped.
fn forwarded_args_for_engine<I: IntoIterator<Item = OsString>>(
    argv: I,
    engine: &Engine,
) -> Result<(Vec<OsString>, Vec<String>)> {
    let dropped_tokens = drop_reld_dispatch_tokens(argv);
    if !engine.native && matches!(engine.format, BridgeTarget::Elf | BridgeTarget::MinGw) {
        let expanded = expand_and_strip_driver_lto(dropped_tokens, 0)?;
        return Ok(strip_reld_only_flags(expanded));
    }
    Ok(strip_reld_only_flags(dropped_tokens))
}

/// Derives Nix RUNPATH `-rpath` flags from the forwarded argv, mirroring nixpkgs'
/// `ld-wrapper.sh`. The native engine performs this derivation while parsing (see
/// `ElfArgs::add_nix_rpath_entries`); a link that routes to lld never goes through that
/// parser, so the bridge must derive the same store RUNPATH and forward it explicitly.
/// Without this, every routed link (LTO, `-z text`, `+crt-static`, …) on NixOS loses the
/// store RUNPATH — the exact rust-lang/rust#162781 failure reld exists to fix.
fn nix_rpath_flags(argv: &[OsString]) -> Vec<OsString> {
    let dont_set = std::env::var("NIX_DONT_SET_RPATH").is_ok_and(|v| v == "1");
    let nix_store = std::env::var("NIX_STORE").unwrap_or_else(|_| "/nix/store".to_owned());
    nix_rpath_flags_with(argv, &nix_store, dont_set)
}

/// Derivation core, split out so it can be exercised directly in tests without touching the
/// environment (mirrors `ElfArgs::add_nix_rpath_entries_with`).
fn nix_rpath_flags_with(argv: &[OsString], nix_store: &str, dont_set_rpath: bool) -> Vec<OsString> {
    if dont_set_rpath {
        return Vec::new();
    }
    let store = Path::new(nix_store);

    let mut lib_dirs: Vec<PathBuf> = Vec::new();
    let mut file_parents: Vec<PathBuf> = Vec::new();
    let mut requested: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        let Some(s) = arg.to_str() else { continue };
        if s == "-L" {
            if let Some(dir) = iter.next() {
                lib_dirs.push(PathBuf::from(dir));
            }
        } else if let Some(dir) = s.strip_prefix("-L") {
            if !dir.is_empty() {
                lib_dirs.push(PathBuf::from(dir));
            }
        } else if s == "-l" {
            if let Some(name) = iter.next().and_then(|n| n.to_str()) {
                requested.insert(format!("lib{name}.so"));
            }
        } else if let Some(name) = s.strip_prefix("-l:") {
            requested.insert(name.to_owned());
        } else if let Some(name) = s.strip_prefix("-l") {
            if !name.is_empty() {
                requested.insert(format!("lib{name}.so"));
            }
        } else if Path::new(s)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("so"))
            || s.contains(".so.")
        {
            let path = Path::new(s);
            if let Some(name) = path.file_name() {
                requested.insert(name.to_string_lossy().into_owned());
            }
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                file_parents.push(parent.to_path_buf());
            }
        }
    }

    if requested.is_empty() {
        return Vec::new();
    }

    let mut candidate_dirs: Vec<&Path> = lib_dirs.iter().map(|p| p.as_path()).collect();
    candidate_dirs.extend(file_parents.iter().map(|p| p.as_path()));

    let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
    let mut flags = Vec::new();
    for dir in candidate_dirs {
        if dir != store && !dir.starts_with(store) {
            continue;
        }
        if !seen.insert(dir) {
            continue;
        }
        let provides = requested
            .iter()
            .any(|name| std::fs::symlink_metadata(dir.join(name)).is_ok());
        if provides {
            flags.push(OsString::from("-rpath"));
            flags.push(OsString::from(dir.to_string_lossy().into_owned()));
        }
    }
    flags
}

fn is_driver_lto_flag(arg: &str) -> bool {
    // GNU flags are case-sensitive (agents/docs/routing-maintenance.md).
    arg == "-flto" || arg == "--flto" || arg.starts_with("-flto=") || arg.starts_with("--flto=")
}

fn expand_and_strip_driver_lto(
    args: impl IntoIterator<Item = OsString>,
    response_depth: usize,
) -> Result<Vec<OsString>> {
    let mut forwarded = Vec::new();
    for arg in args {
        if let Some(arg) = arg.to_str() {
            if is_driver_lto_flag(arg) {
                continue;
            }
            if let Some(path) = arg.strip_prefix('@') {
                if response_depth >= 16 {
                    bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
                }
                let nested = crate::args::read_args_from_file(Path::new(path))?
                    .into_iter()
                    .map(OsString::from);
                forwarded.extend(expand_and_strip_driver_lto(nested, response_depth + 1)?);
                continue;
            }
        }
        forwarded.push(arg);
    }
    Ok(forwarded)
}

/// Extracts the `--engine=<name>` override from the process argv, if present. Skips `argv[0]`
/// (the program path) to mirror `forwarded_args`, so a program invoked from a path that happens
/// to start with `--engine=` can never be misread as an override.
fn engine_override_from_argv<'a, I: IntoIterator<Item = &'a OsString>>(argv: I) -> Option<String> {
    argv.into_iter().skip(1).find_map(|arg| {
        arg.to_str()
            .and_then(|s| s.strip_prefix(ENGINE_FLAG_PREFIX))
            .map(str::to_owned)
    })
}

/// Builds the full command line for the child linker: the forwarded args, prefixed with
/// `-flavor <target>` when the discovered linker is the multi-flavor `rust-lld` dispatcher.
fn child_command_line(linker: &Path, engine: &Engine, forwarded: Vec<OsString>) -> Vec<OsString> {
    let mut args = Vec::with_capacity(forwarded.len() + 2);
    if needs_flavor_prefix(linker) {
        args.push(OsString::from("-flavor"));
        args.push(OsString::from(engine.rust_lld_flavor));
    }
    args.extend(forwarded);
    args
}

/// Runs the bridge: discovers a linker capable of the given target format and execs it with the
/// pass-through argv, bypassing reld's own argument parser entirely.
///
/// `argv` is the full process argv, including `argv[0]`; `argv[0]` is dropped and the rest is
/// forwarded as the linker's command line (with `-flavor <target>` prepended if the discovered
/// linker needs it).
pub fn run_bridge<I: IntoIterator<Item = OsString>>(argv: I, route: Route) -> Result<()> {
    let argv: Vec<OsString> = argv.into_iter().collect();
    if !route.is_bridge() {
        bail!("Internal error: native engine passed to the subprocess bridge");
    }
    let engine = route.engine;

    let (mut forwarded, dropped) = forwarded_args_for_engine(argv, engine)?;
    if let Some(emulation) = route.mingw_emulation {
        // No `-m i386pe*` on argv (`Route`'s doc comment on `mingw_emulation` explains when this
        // happens): tell `ld.lld`'s MinGW driver which PE/COFF flavor to emit.
        forwarded.splice(0..0, [OsString::from("-m"), OsString::from(emulation)]);
    }
    if engine.format == BridgeTarget::Elf {
        // If `--nix-rpath=off` was stripped, the bridge must not derive Nix RUNPATH flags either:
        // the bridge now honors what it strips, rather than re-deriving something the caller asked
        // it not to set. Nix RUNPATH derivation is ELF-specific (nixpkgs' `ld-wrapper.sh` targets
        // ELF `DT_RUNPATH`); other formats have no such convention.
        let nix_rpath_off = dropped
            .iter()
            .rev()
            .find_map(|entry| {
                entry
                    .strip_prefix("--nix-rpath=")
                    .or_else(|| entry.strip_prefix("--nix-rpath "))
            })
            .is_some_and(|value| value == "off");
        if !nix_rpath_off {
            // A routed link must keep the Nix store RUNPATH the native engine would have derived.
            forwarded.extend(nix_rpath_flags(&forwarded));
        }
    }
    #[cfg(feature = "llvm-ld")]
    if let Some(exit_code) = try_llvm_ld_in_process(engine, &route, &forwarded, &dropped) {
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return Ok(());
    }

    let linker = discover_linker(engine)?;
    let child_args = child_command_line(&linker, engine, forwarded);

    if route_logging_enabled() {
        eprintln!(
            "reld: engine={} (bridge, reason={}) -> {}",
            engine.name,
            route.reason.label(),
            linker.display()
        );
        if let Some(note) = stripped_options_note(engine.name, &dropped) {
            eprintln!("{note}");
        }
    }

    let mut command = std::process::Command::new(&linker);
    command.args(child_args);
    if let Some((variable, value)) =
        toolchain_dylib_search_env(&linker, std::env::var_os(TOOLCHAIN_DYLIB_SEARCH.0))
    {
        command.env(variable, value);
    }

    command.stdin(std::process::Stdio::inherit());
    command.stdout(std::process::Stdio::inherit());
    command.stderr(std::process::Stdio::inherit());

    let status = command
        .status()
        .with_context(|| format!("Failed to spawn bridge linker `{}`", linker.display()))?;

    if !status.success() {
        // Propagate a signal-killed child as `128 + signal`, naming the engine and signal, so a
        // crashed bridge (ld.lld, lld-link, ld64.lld) is diagnosable rather than collapsing to a
        // bare exit 1 (reld#123 D6).
        if let Some(signal) = crate::platforms::process::exit_signal(&status) {
            crate::diagnostic::emit(
                crate::diagnostic::Severity::Error,
                &format_args!("{} bridge terminated by signal {signal}", engine.name),
            );
            std::process::exit(128 + signal);
        }
        // Match how reld currently exits (see `report_error_and_exit`): terminate the process
        // directly with the child's exit code, rather than propagating a `Result` error that
        // would go through our own error formatting.
        let code = status.code().unwrap_or(1);
        std::process::exit(code);
    }

    Ok(())
}

/// Offer a COFF or MinGW link to an in-process llvm-ld-coff (reld#96). Returns the exit code when llvm-ld
/// ran the link, or `None` when the subprocess bridge should take it instead.
///
/// An explicit `RELD_BRIDGE_LINKER` names the linker to run, so it bypasses llvm-ld.
#[cfg(feature = "llvm-ld")]
fn try_llvm_ld_in_process(
    engine: &Engine,
    route: &Route,
    forwarded: &[OsString],
    dropped: &[String],
) -> Option<i32> {
    use crate::llvm_ld::Driver;
    use crate::llvm_ld::Outcome;

    let driver = if engine.name == COFF_LLD_ENGINE.name {
        Driver::WinLink
    } else if engine.name == MINGW_LLD_ENGINE.name {
        Driver::MinGw
    } else {
        return None;
    };
    if std::env::var_os(RELD_BRIDGE_LINKER_ENV).is_some() {
        return None;
    }
    match crate::llvm_ld::invoke(driver, forwarded) {
        Outcome::Linked { exit_code, library } => {
            if route_logging_enabled() {
                eprintln!(
                    "reld: engine={} (in-process llvm-ld-coff, reason={}) -> {}",
                    engine.name,
                    route.reason.label(),
                    library.display()
                );
                if let Some(note) = stripped_options_note(engine.name, dropped) {
                    eprintln!("{note}");
                }
            }
            Some(exit_code)
        }
        Outcome::Unavailable { reason } => {
            if route_logging_enabled() {
                eprintln!("reld: llvm-ld-coff not used ({reason}); using the subprocess bridge");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Environment variable mutation isn't thread-safe, so serialize the tests that touch
    // linker-control environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: Tests are serialized via ENV_LOCK, so no other thread observes this
            // process's environment concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: See above.
            unsafe { std::env::remove_var(self.key) };
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "reld-bridge-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        path
    }

    /// Creates an empty file at a unique temp path. Returns a guard that removes it on drop.
    struct TempFile(PathBuf);

    impl TempFile {
        fn create(name: &str) -> Self {
            Self::create_with_contents(name, b"")
        }

        fn create_with_contents(name: &str, contents: &[u8]) -> Self {
            let path = unique_temp_path(name);
            std::fs::write(&path, contents).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// reld#206: a toolchain `rust-lld` gets the toolchain's shared-library directory prepended to
    /// the host loader search path, ahead of whatever the caller already had there.
    #[test]
    fn toolchain_rust_lld_gets_toolchain_dylib_dir_on_loader_path() {
        let root = unique_temp_path("toolchain");
        let bin = root
            .join("lib")
            .join("rustlib")
            .join("aarch64-apple-darwin")
            .join("bin");
        std::fs::create_dir_all(bin.join("gcc-ld")).unwrap();
        let dylib_dir = root.join(TOOLCHAIN_DYLIB_SEARCH.1);
        std::fs::create_dir_all(&dylib_dir).unwrap();
        let (variable, _) = TOOLCHAIN_DYLIB_SEARCH;

        for linker in [
            bin.join(format!("rust-lld{EXE_SUFFIX}")),
            bin.join("gcc-ld").join(format!("ld64.lld{EXE_SUFFIX}")),
        ] {
            assert_eq!(toolchain_root_of(&linker), Some(root.as_path()));
            let (name, value) = toolchain_dylib_search_env(&linker, None).unwrap();
            assert_eq!(name, variable);
            assert_eq!(value, dylib_dir.clone().into_os_string());

            let existing = unique_temp_path("existing");
            let current = std::env::join_paths([existing.clone()]).unwrap();
            let (_, value) = toolchain_dylib_search_env(&linker, Some(current)).unwrap();
            assert_eq!(
                std::env::split_paths(&value).collect::<Vec<_>>(),
                vec![dylib_dir.clone(), existing]
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn non_toolchain_linker_gets_no_loader_path_entry() {
        let dir = unique_temp_path("plain");
        std::fs::create_dir_all(&dir).unwrap();
        let linker = dir.join(format!("ld64.lld{EXE_SUFFIX}"));
        assert_eq!(toolchain_root_of(&linker), None);
        assert_eq!(toolchain_dylib_search_env(&linker, None), None);
        // A toolchain-shaped path whose library directory does not exist adds nothing either.
        let missing = dir
            .join("lib")
            .join("rustlib")
            .join("x")
            .join("bin")
            .join("rust-lld");
        assert_eq!(toolchain_root_of(&missing), Some(dir.as_path()));
        assert_eq!(toolchain_dylib_search_env(&missing, None), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_override_returns_set_path() {
        let _lock = ENV_LOCK.lock().unwrap();
        let fake_linker = TempFile::create("env-override-linker");
        let _guard = EnvVarGuard::set(RELD_BRIDGE_LINKER_ENV, fake_linker.path().to_str().unwrap());

        let discovered = discover_linker(Engine::default_for(BridgeTarget::Coff)).unwrap();
        assert_eq!(discovered, fake_linker.path());

        let discovered = discover_linker(Engine::default_for(BridgeTarget::MachO)).unwrap();
        assert_eq!(discovered, fake_linker.path());

        let discovered = discover_linker(Engine::find("lld").unwrap()).unwrap();
        assert_eq!(discovered, fake_linker.path());
    }

    #[test]
    fn env_override_with_nonexistent_path_errors() {
        let _lock = ENV_LOCK.lock().unwrap();
        let missing = unique_temp_path("does-not-exist");
        let _guard = EnvVarGuard::set(RELD_BRIDGE_LINKER_ENV, missing.to_str().unwrap());

        let err = discover_linker(Engine::default_for(BridgeTarget::Coff)).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(RELD_BRIDGE_LINKER_ENV),
            "unexpected error message: {message}"
        );
        assert!(
            message.contains("does not exist"),
            "unexpected error message: {message}"
        );
    }

    #[test]
    fn successful_invocation_log_records_exact_output_and_route() {
        let _lock = ENV_LOCK.lock().unwrap();
        let log = TempFile::create("successful-invocation.jsonl");
        let _guard = EnvVarGuard::set(RELD_INVOCATION_LOG_ENV, log.path().to_str().unwrap());
        let argv = vec![
            OsString::from("reld-link"),
            OsString::from("input.obj"),
            OsString::from("/OUT:C:/build/consumer.exe"),
        ];
        let route = Route {
            engine: &COFF_LLD_ENGINE,
            reason: SelectionReason::Default,
            mingw_emulation: None,
        };

        log_successful_invocation(&argv, route).unwrap();

        let contents = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let record: serde_yaml::Value = serde_yaml::from_str(lines[0]).unwrap();
        assert_eq!(record["schema"], 1);
        assert_eq!(record["status"], "success");
        assert_eq!(record["engine"], "lld-link");
        assert_eq!(record["route_kind"], "bridge");
        assert_eq!(record["reason"], "default");
        assert_eq!(record["output"], "C:/build/consumer.exe");
        assert_eq!(record["arguments"][0], "input.obj");
        assert_eq!(record["arguments"][1], "/OUT:C:/build/consumer.exe");
        assert!(record["process_id"].as_u64().unwrap() > 0);
        assert!(!record["working_directory"].as_str().unwrap().is_empty());
    }

    #[test]
    fn requested_output_preserves_gnu_and_coff_path_spelling() {
        let gnu = vec![
            OsString::from("reld"),
            OsString::from("-o"),
            OsString::from("Build/Mixed Case/app"),
        ];
        assert_eq!(
            requested_output(&gnu, BridgeTarget::Elf)
                .unwrap()
                .as_deref(),
            Some("Build/Mixed Case/app")
        );

        let coff = vec![
            OsString::from("reld-link"),
            OsString::from("-OUT:C:/Build/Mixed Case/app.exe"),
        ];
        assert_eq!(
            requested_output(&coff, BridgeTarget::Coff)
                .unwrap()
                .as_deref(),
            Some("C:/Build/Mixed Case/app.exe")
        );

        let response = TempFile::create_with_contents(
            "coff-output-response",
            br#"input.obj "/OUT:C:\Build\Mixed Case\response.exe" /DEBUG"#,
        );
        let response_argv = vec![
            OsString::from("reld-link"),
            OsString::from(format!("@{}", response.path().display())),
        ];
        assert_eq!(
            requested_output(&response_argv, BridgeTarget::Coff)
                .unwrap()
                .as_deref(),
            Some(r"C:\Build\Mixed Case\response.exe")
        );
    }

    #[test]
    fn response_file_quoting_matches_on_native_and_classifier_paths() {
        // mold's response-file-quoting.sh: a quote can open and close mid-token (they
        // concatenate around it), and a backslash escapes exactly the next character.
        let mold_quoting = TempFile::create_with_contents(
            "mold-response-file-quoting",
            b"/t/a.o\n/t\"\\/b.\"\\o\n\\foo\\bar\n",
        );
        let tokens = crate::args::read_args_from_file(mold_quoting.path()).unwrap();
        assert_eq!(tokens, vec!["/t/a.o", "/t/b.o", "foobar"]);

        let icf_quoting = TempFile::create_with_contents(
            "icf-response-file-quoting",
            br#"-o 'out'"put".so --i"cf=all""#,
        );
        let tokens = crate::args::read_args_from_file(icf_quoting.path()).unwrap();
        assert_eq!(tokens, vec!["-o", "output.so", "--icf=all"]);

        let icf_arg = format!("@{}", icf_quoting.path().display());
        assert_eq!(
            requested(&["reld", icf_arg.as_str()]),
            vec![Capability::Icf]
        );

        let argv = vec![OsString::from("reld"), OsString::from(icf_arg)];
        assert_eq!(
            requested_output(&argv, BridgeTarget::Elf)
                .unwrap()
                .as_deref(),
            Some("output.so")
        );
    }

    #[test]
    fn needs_flavor_prefix_for_rust_lld() {
        // Forward-slash paths so `file_stem()` behaves identically on every host (backslashes
        // are not path separators off Windows). The host-native separator form is covered below.
        assert!(needs_flavor_prefix(Path::new("/some/path/rust-lld")));
        assert!(needs_flavor_prefix(Path::new("/some/path/rust-lld.exe")));
        assert!(needs_flavor_prefix(Path::new("RUST-LLD")));
        let native = format!(
            "C:{s}some{s}path{s}rust-lld.exe",
            s = std::path::MAIN_SEPARATOR
        );
        assert!(needs_flavor_prefix(Path::new(&native)));
    }

    #[test]
    fn no_flavor_prefix_for_concrete_drivers() {
        assert!(!needs_flavor_prefix(Path::new("/some/path/lld-link")));
        assert!(!needs_flavor_prefix(Path::new("/some/path/lld-link.exe")));
        assert!(!needs_flavor_prefix(Path::new("/some/path/ld64.lld")));
        let native = format!(
            "C:{s}some{s}path{s}lld-link.exe",
            s = std::path::MAIN_SEPARATOR
        );
        assert!(!needs_flavor_prefix(Path::new(&native)));
    }

    #[test]
    fn not_found_message_names_env_var() {
        // The "no linker discoverable" error must always tell the user about the override knob,
        // for both bridge targets.
        for engine in [
            Engine::find("lld").unwrap(),
            Engine::default_for(BridgeTarget::Coff),
            Engine::default_for(BridgeTarget::MachO),
        ] {
            let message = not_found_message(engine);
            assert!(
                message.contains(RELD_BRIDGE_LINKER_ENV),
                "unexpected error message: {message}"
            );
        }
    }

    #[test]
    fn forwarded_args_drops_only_argv0_for_name_route() {
        let argv = [
            OsString::from("reld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_leading_flavor_pair() {
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn flavor_route_to_rust_lld_yields_exactly_one_flavor_pair_coff() {
        // The bug this guards against: a `-flavor link` invocation of reld, bridged to the
        // multi-flavor `rust-lld`, must produce a single `-flavor link` — never a doubled pair
        // that leaks into the COFF driver.
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("/OUT:a.exe"),
        ];
        let child = child_command_line(
            Path::new("/tc/rust-lld.exe"),
            Engine::default_for(BridgeTarget::Coff),
            forwarded_args(argv),
        );
        assert_eq!(
            child,
            vec![
                OsString::from("-flavor"),
                OsString::from("link"),
                OsString::from("/OUT:a.exe"),
            ]
        );
    }

    #[test]
    fn flavor_route_to_rust_lld_yields_exactly_one_flavor_pair_macho() {
        // Same guard as above, but for the Mach-O target: a `-flavor darwin` invocation of reld,
        // bridged to the multi-flavor `rust-lld`, must produce a single `-flavor darwin`.
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("darwin"),
            OsString::from("-o"),
            OsString::from("a.out"),
        ];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::default_for(BridgeTarget::MachO),
            forwarded_args(argv),
        );
        assert_eq!(
            child,
            vec![
                OsString::from("-flavor"),
                OsString::from("darwin"),
                OsString::from("-o"),
                OsString::from("a.out"),
            ]
        );
    }

    #[test]
    fn coff_flavor_prefix_is_link() {
        let forwarded = vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::default_for(BridgeTarget::Coff),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("link"));
    }

    #[test]
    fn macho_flavor_prefix_is_darwin() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::default_for(BridgeTarget::MachO),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("darwin"));
    }

    #[test]
    fn elf_flavor_prefix_is_gnu() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::find("lld").unwrap(),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("gnu"));
    }

    #[test]
    fn lld_link_route_has_no_flavor_prefix() {
        let forwarded = vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")];
        let expected = forwarded.clone();
        let child = child_command_line(
            Path::new("/tc/gcc-ld/lld-link.exe"),
            Engine::default_for(BridgeTarget::Coff),
            forwarded,
        );
        assert_eq!(child, expected);
    }

    #[test]
    fn ld64_lld_route_has_no_flavor_prefix() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let expected = forwarded.clone();
        let child = child_command_line(
            Path::new("/tc/gcc-ld/ld64.lld"),
            Engine::default_for(BridgeTarget::MachO),
            forwarded,
        );
        assert_eq!(child, expected);
    }

    #[test]
    fn select_engine_default_for_coff_is_lld_link() {
        let engine = select_engine(BridgeTarget::Coff, None, &[]).unwrap();
        assert_eq!(engine.name, "lld-link");
        assert_eq!(engine.format, BridgeTarget::Coff);
    }

    #[test]
    fn select_engine_default_for_macho_is_ld64_lld() {
        let engine = select_engine(BridgeTarget::MachO, None, &[]).unwrap();
        assert_eq!(engine.name, "ld64.lld");
        assert_eq!(engine.format, BridgeTarget::MachO);
    }

    #[test]
    fn select_engine_valid_override_matches_target() {
        let engine = select_engine(BridgeTarget::Coff, Some("lld-link"), &[]).unwrap();
        assert_eq!(engine.name, "lld-link");

        let engine = select_engine(BridgeTarget::MachO, Some("ld64.lld"), &[]).unwrap();
        assert_eq!(engine.name, "ld64.lld");
    }

    #[test]
    fn select_engine_unknown_name_errors_listing_valid_engines() {
        let err = select_engine(BridgeTarget::Coff, Some("bogus"), &[]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("bogus"), "unexpected message: {message}");
        assert!(
            message.contains("lld-link"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("ld64.lld"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn select_engine_format_mismatch_errors() {
        // ld64.lld links Mach-O; requesting it for a COFF link is a format mismatch.
        let err = select_engine(BridgeTarget::Coff, Some("ld64.lld"), &[]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("ld64.lld"),
            "unexpected message: {message}"
        );
        assert!(message.contains("COFF"), "unexpected message: {message}");

        // Symmetric direction: lld-link links COFF; requesting it for a Mach-O link mismatches.
        let err = select_engine(BridgeTarget::MachO, Some("lld-link"), &[]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("lld-link"),
            "unexpected message: {message}"
        );
        assert!(message.contains("Mach-O"), "unexpected message: {message}");
    }

    #[test]
    fn elf_defaults_to_native_reld() {
        let route = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("foo.o")],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap();
        assert_eq!(route.engine.name, "reld");
        assert!(!route.is_bridge());
        assert_eq!(route.reason, SelectionReason::Default);
    }

    #[test]
    fn explicit_native_override_can_accept_capabilities_with_ignore_policy() {
        let route = select_route_with_policy(
            &[
                OsString::from("ld.reld"),
                OsString::from("-flto"),
                OsString::from("foo.o"),
            ],
            BridgeTarget::Elf.into(),
            Some("reld"),
            true,
            false,
        )
        .unwrap();
        assert_eq!(route.engine.name, "reld");
        assert!(!route.is_bridge());
        assert_eq!(route.reason, SelectionReason::OverrideEnv);
    }

    #[test]
    fn ignore_policy_does_not_suppress_conflicting_lld_override() {
        let error = select_route_with_policy(
            &[
                OsString::from("ld.reld"),
                OsString::from("--validate-output"),
            ],
            BridgeTarget::Elf.into(),
            Some("lld"),
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("reld-only validation"));
    }

    #[test]
    fn ignore_policy_does_not_suppress_unknown_override() {
        let error = select_route_with_policy(
            &[OsString::from("ld.reld"), OsString::from("foo.o")],
            BridgeTarget::Elf.into(),
            Some("bogus"),
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Unknown engine `bogus`"));
    }

    #[test]
    fn elf_lto_spellings_route_to_lld() {
        for flag in [
            "-flto",
            "-flto=thin",
            "--flto=full",
            "--plugin=/tool/LLVMgold.so",
            "-plugin",
        ] {
            let route = select_route_with_env(
                &[
                    OsString::from("ld.reld"),
                    OsString::from(flag),
                    OsString::from("foo.o"),
                ],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
            assert!(route.is_bridge(), "flag {flag}");
            assert!(matches!(route.reason, SelectionReason::Capability(_)));
        }
    }

    #[test]
    fn elf_icf_routes_to_lld_but_disabled_icf_stays_native() {
        for flag in ["--icf=all", "--icf=safe"] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
        }

        let native = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("--icf=none")],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap();
        assert_eq!(native.engine.name, "reld");
    }

    #[test]
    fn unsupported_native_semantics_route_to_lld() {
        for flag in ["--discard-all", "-x", "--fix-cortex-a53-843419"] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
        }
    }

    #[test]
    fn no_undefined_version_stays_native() {
        // rustc emits `--no-undefined-version` on every cdylib/dylib/proc-macro
        // link; routing it to lld would push all of those off the fast engine
        // (reld#123 §B). It must stay native.
        for flag in [
            "--no-undefined-version",
            "-no-undefined-version",
            "--undefined-version",
            "-undefined-version",
        ] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "reld", "flag {flag}");
        }
    }

    #[test]
    fn fatal_warnings_stays_native() {
        // `--fatal-warnings` is diagnostics plumbing implemented natively (reld#123 Phase 2);
        // it must not route to lld.
        for flag in [
            "--fatal-warnings",
            "--no-fatal-warnings",
            "--color-diagnostics",
            "--no-color-diagnostics",
        ] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "reld", "flag {flag}");
        }
    }

    #[test]
    fn z_text_routes_to_lld_and_notext_stays_native() {
        // `-z text` errors on DT_TEXTREL; the native engine never errors on text
        // relocations, so it routes to lld. `-z notext` is reld's default
        // behavior and stays native (reld#123 Phase 0).
        for argv in [
            &["ld.reld", "-z", "text"][..],
            &["ld.reld", "-ztext"][..],
            &["ld.reld", "-z=text"][..],
        ] {
            let route = select_route_with_env(
                &argv.iter().map(OsString::from).collect::<Vec<_>>(),
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "argv {argv:?}");
        }

        for argv in [
            &["ld.reld", "-z", "notext"][..],
            &["ld.reld", "-znotext"][..],
        ] {
            let route = select_route_with_env(
                &argv.iter().map(OsString::from).collect::<Vec<_>>(),
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "reld", "argv {argv:?}");
        }
    }

    #[test]
    fn nix_rpath_flags_derive_store_dir() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libfoo.so"), []).unwrap();
        let store = dir.path().to_string_lossy().into_owned();
        let lib_str = lib.to_string_lossy().into_owned();

        let argv = [
            OsString::from("ld.reld"),
            OsString::from("-L"),
            OsString::from(&lib_str),
            OsString::from("-lfoo"),
            OsString::from("foo.o"),
        ];
        let flags = nix_rpath_flags_with(&argv, &store, false);
        assert_eq!(
            flags,
            vec![OsString::from("-rpath"), OsString::from(&lib_str)]
        );
    }

    #[test]
    fn nix_rpath_flags_skip_when_dont_set() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libfoo.so"), []).unwrap();
        let store = dir.path().to_string_lossy().into_owned();
        let lib_str = lib.to_string_lossy().into_owned();

        let argv = [
            OsString::from("ld.reld"),
            OsString::from("-L"),
            OsString::from(&lib_str),
            OsString::from("-lfoo"),
        ];
        assert!(nix_rpath_flags_with(&argv, &store, true).is_empty());
    }

    #[test]
    fn stripped_reld_only_flags_are_dropped() {
        let argv = [
            OsString::from("--time"),
            OsString::from("--mmap-output-file"),
            OsString::from("--threads=4"),
            OsString::from("-L"),
            OsString::from("/lib"),
            OsString::from("-lfoo"),
            OsString::from("foo.o"),
        ];
        let out = strip_reld_only_flags(argv.to_vec()).0;
        let out: Vec<&str> = out.iter().filter_map(|a| a.to_str()).collect();
        assert_eq!(out, ["-L", "/lib", "-lfoo", "foo.o"]);
    }

    #[test]
    fn stripped_time_drops_following_value() {
        let argv = [
            OsString::from("--time"),
            OsString::from("parse"),
            OsString::from("foo.o"),
        ];
        let out = strip_reld_only_flags(argv.to_vec()).0;
        let out: Vec<&str> = out.iter().filter_map(|a| a.to_str()).collect();
        assert_eq!(out, ["foo.o"]);
    }

    #[test]
    fn sym_info_forces_native_and_conflicts_with_lld_route() {
        let native = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("--sym-info=x")],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap();
        assert_eq!(native.engine.name, "reld");

        // `--sym-info` is NativeControl (native-only); combined with a routed requirement it
        // must be a loud conflict, not a silent drop.
        let err = select_route_with_env(
            &[
                OsString::from("ld.reld"),
                OsString::from("--sym-info=x"),
                OsString::from("--icf=all"),
            ],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("No bundled ELF engine supports"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn linker_plugin_inside_response_file_routes_to_lld() {
        let response =
            TempFile::create_with_contents("lto-response", b"--plugin=/tool/LLVMgold.so foo.o");
        let route = select_route_with_env(
            &[
                OsString::from("ld.reld"),
                OsString::from(format!("@{}", response.path().display())),
            ],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::Capability("flag:--plugin"));
    }

    #[test]
    fn non_elf_response_files_are_forwarded_without_gnu_or_utf8_parsing() {
        // UTF-16LE response data with Windows quoting. This must remain opaque to the router;
        // lld-link/ld64.lld own their response-file grammar and encoding.
        let response = TempFile::create_with_contents(
            "windows-utf16-response",
            &[
                0xff, 0xfe, b'/', 0, b'O', 0, b'U', 0, b'T', 0, b':', 0, b'"', 0, b'a', 0, b' ', 0,
                b'b', 0, b'.', 0, b'e', 0, b'x', 0, b'e', 0, b'"', 0,
            ],
        );
        let response_arg = OsString::from(format!("@{}", response.path().display()));

        for target in [BridgeTarget::Coff, BridgeTarget::MachO] {
            let argv = [OsString::from("reld"), response_arg.clone()];
            let route = select_route_with_env(&argv, target.into(), None).unwrap();
            assert_eq!(route.engine, Engine::default_for(target));
            assert_eq!(
                forwarded_args_for_engine(argv, route.engine).unwrap().0,
                vec![response_arg.clone()]
            );
        }
    }

    #[test]
    fn elf_lld_strips_driver_lto_flag_but_keeps_plugin_flag() {
        let argv = [
            OsString::from("ld.reld"),
            OsString::from("--engine=lld"),
            OsString::from("-flto=thin"),
            OsString::from("--plugin=/tool/LLVMgold.so"),
            OsString::from("foo.o"),
        ];
        assert_eq!(
            forwarded_args_for_engine(argv, Engine::find("lld").unwrap())
                .unwrap()
                .0,
            vec![
                OsString::from("--plugin=/tool/LLVMgold.so"),
                OsString::from("foo.o"),
            ]
        );
    }

    #[test]
    fn elf_lld_expands_response_files_and_strips_nested_driver_lto_flag() {
        let nested = TempFile::create_with_contents(
            "nested-lto-response",
            b"-flto=thin --build-id=sha1 nested.o",
        );
        let nested_path = nested.path().to_string_lossy().replace('\\', "/");
        let outer = TempFile::create_with_contents(
            "outer-lto-response",
            format!("@{nested_path} outer.o").as_bytes(),
        );
        let argv = [
            OsString::from("ld.reld"),
            OsString::from(format!("@{}", outer.path().display())),
        ];

        let route = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(
            forwarded_args_for_engine(argv, route.engine).unwrap().0,
            vec![
                OsString::from("--build-id=sha1"),
                OsString::from("nested.o"),
                OsString::from("outer.o"),
            ]
        );
    }

    #[test]
    fn explicit_engine_flag_takes_precedence_over_environment() {
        let route = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("--engine=lld")],
            BridgeTarget::Elf.into(),
            Some("reld"),
        )
        .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::OverrideFlag);
    }

    #[test]
    fn environment_can_force_elf_lld() {
        let route = select_route_with_env(
            &[OsString::from("ld.reld")],
            BridgeTarget::Elf.into(),
            Some("lld"),
        )
        .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::OverrideEnv);
    }

    #[test]
    fn forcing_native_reld_for_lto_is_a_clear_error() {
        let error = select_route_with_env(
            &[
                OsString::from("ld.reld"),
                OsString::from("--engine=reld"),
                OsString::from("-flto"),
            ],
            BridgeTarget::Elf.into(),
            None,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("reld"), "unexpected message: {message}");
        assert!(message.contains("LTO"), "unexpected message: {message}");
        assert!(message.contains("-flto"), "unexpected message: {message}");
    }

    #[test]
    fn forwarded_args_strips_engine_flag_at_start() {
        let argv = [
            OsString::from("reld"),
            OsString::from("--engine=lld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_engine_flag_in_middle() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("--engine=ld64.lld"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_engine_flag_at_end() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
            OsString::from("--engine=lld-link"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_unchanged_when_no_engine_flag() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_both_flavor_pair_and_engine_flag() {
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("--engine=lld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn engine_override_from_argv_finds_token_anywhere() {
        let argv = vec![
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("--engine=ld64.lld"),
        ];
        assert_eq!(
            engine_override_from_argv(&argv),
            Some("ld64.lld".to_string())
        );
    }

    #[test]
    fn engine_override_from_argv_none_when_absent() {
        let argv = vec![OsString::from("reld"), OsString::from("/OUT:a.exe")];
        assert_eq!(engine_override_from_argv(&argv), None);
    }

    fn requested(argv: &[&str]) -> Vec<Capability> {
        let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
        requested_capabilities(&argv)
            .unwrap()
            .into_iter()
            .map(|requirement| requirement.capability)
            .collect()
    }

    #[test]
    fn classifier_is_case_sensitive() {
        // `-x` is `--discard-all` (routes to lld); `-X` is `--discard-locals`
        // (a native default) and must not route.
        assert_eq!(requested(&["reld", "-x"]), vec![Capability::DiscardAll]);
        assert!(requested(&["reld", "-X"]).is_empty());

        // GNU and ld64 flag spellings are case-sensitive (agents/docs/routing-maintenance.md); a
        // differently-cased spelling of a routed flag is unrecognized by the classifier, and the
        // native parser -- not the router -- owns rejecting it.
        for flag in [
            "--ICF=all",
            "--Icf=safe",
            "-FLTO",
            "--FLTO=thin",
            "--PLUGIN=/x.so",
            "-Plugin-opt=O0",
            "--Discard-All",
            "--FIX-CORTEX-A53-843419",
            "-zTEXT",
            "--Validate-Output",
        ] {
            assert!(requested(&["reld", flag]).is_empty(), "flag {flag}");
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "reld", "flag {flag}");
            assert_eq!(route.reason, SelectionReason::Default, "flag {flag}");
        }
    }

    #[test]
    fn static_group_flags_stay_native() {
        // Pins that clang's `-static` archive-grouping idiom stays on the fast engine, in both
        // its `--start-group`/`--end-group` and `-(`/`-)` spellings. The inputs are relative and
        // deliberately don't exist: this exercises routing only, never a real link.
        for argv in [
            &[
                "ld.reld",
                "-static",
                "-o",
                "a.out",
                "crt1.o",
                "crti.o",
                "crtbeginT.o",
                "-L/nonexistent/gcc",
                "main.o",
                "--start-group",
                "-lgcc",
                "-lgcc_eh",
                "-lc",
                "--end-group",
                "crtend.o",
                "crtn.o",
            ][..],
            &[
                "ld.reld",
                "-static",
                "-o",
                "a.out",
                "crt1.o",
                "crti.o",
                "crtbeginT.o",
                "-L/nonexistent/gcc",
                "main.o",
                "-(",
                "-lgcc",
                "-lgcc_eh",
                "-lc",
                "-)",
                "crtend.o",
                "crtn.o",
            ][..],
        ] {
            assert!(requested(argv).is_empty(), "argv {argv:?}");
            let route = select_route_with_env(
                &argv.iter().map(OsString::from).collect::<Vec<_>>(),
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "reld", "argv {argv:?}");
            assert_eq!(route.reason, SelectionReason::Default, "argv {argv:?}");
        }
    }

    #[test]
    fn plugin_opt_and_bitcode_route_to_lld() {
        // rustc `-C linker-plugin-lto` through clang emits `-plugin-opt=…`
        // without `-plugin`/`-flto`, so bitcode must still route to lld.
        assert_eq!(
            requested(&["reld", "-plugin-opt=O0"]),
            vec![Capability::Lto]
        );
        assert_eq!(
            requested(&["reld", "-plugin-opt", "O0"]),
            vec![Capability::Lto]
        );
        for argv in [
            &["ld.reld", "-plugin-opt=O0", "foo.o"][..],
            &["ld.reld", "-plugin-opt", "O0", "foo.o"][..],
        ] {
            let route = select_route_with_env(
                &argv.iter().map(OsString::from).collect::<Vec<_>>(),
                BridgeTarget::Elf.into(),
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "argv {argv:?}");
            assert_eq!(
                route.reason,
                SelectionReason::Capability("flag:-plugin-opt"),
                "argv {argv:?}"
            );
        }

        // An LTO object with no `-flto`/`-plugin-opt` on the command line must still route to
        // lld: the native engine has no LTO implementation (#123 Phase 0).
        let raw_bitcode =
            TempFile::create_with_contents("raw-bitcode", b"BC\xC0\xDE\x35\x14\x00\x00");
        let wrapped_bitcode =
            TempFile::create_with_contents("wrapped-bitcode", b"\xDE\xC0\x17\x0B\x00\x00\x00\x00");
        for input in [raw_bitcode.path(), wrapped_bitcode.path()] {
            let argv = [
                OsString::from("ld.reld"),
                OsString::from(input.to_str().unwrap()),
            ];
            let route = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap();
            assert_eq!(route.engine.name, "lld", "input {}", input.display());
            assert_eq!(
                route.reason,
                SelectionReason::Capability("input:bitcode"),
                "input {}",
                input.display()
            );
        }

        // A real ELF object (not bitcode) stays native.
        let elf_input =
            TempFile::create_with_contents("not-bitcode-elf", b"\x7fELF\x02\x01\x01\x00");
        let argv = [
            OsString::from("ld.reld"),
            OsString::from(elf_input.path().to_str().unwrap()),
        ];
        let route = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap();
        assert_eq!(route.engine.name, "reld");
    }

    #[test]
    fn reld_only_flags_never_reach_child() {
        // One representative spelling per `Stripped` value kind (see `StrippedValueKind`):
        // `Required` flags use both the `=value` and split `flag value` spellings, `Optional`
        // flags are exercised with an inline `=value` (avoiding the pre-existing ambiguity where
        // a bare `--time`/`--threads` can misidentify a following real input as its value) plus
        // one bare case (`--time`).
        let cases: &[(&str, &[&str])] = &[
            ("thread-count", &["--thread-count", "4"]),
            ("debug-fuel", &["--debug-fuel=10"]),
            ("reld-experiments", &["--reld-experiments=1,_"]),
            ("gc-stats-ignore", &["--gc-stats-ignore", "x.o"]),
            ("nix-rpath", &["--nix-rpath=auto"]),
            ("threads", &["--threads=4"]),
            ("time", &["--time"]),
        ];
        for (name, tokens) in cases {
            // The real input (`foo.o`) precedes the reld-only tokens so a bare `--time` at the
            // end of argv (no following token) cannot swallow it as an optional value.
            let mut argv = vec![
                OsString::from("ld.reld"),
                OsString::from("--icf=all"),
                OsString::from("foo.o"),
            ];
            argv.extend(tokens.iter().map(OsString::from));

            let route = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap();
            assert_eq!(route.engine.name, "lld", "flag {name}");

            let (forwarded, dropped) =
                forwarded_args_for_engine(argv.clone(), route.engine).unwrap();
            assert_eq!(
                forwarded,
                vec![OsString::from("--icf=all"), OsString::from("foo.o")],
                "flag {name}"
            );
            let note = stripped_options_note("lld", &dropped).unwrap();
            assert!(
                note.contains(format!("--{name}").as_str()),
                "flag {name}: note {note}"
            );
        }

        // Nested inside an @file: reld-only flags must be stripped there too, because they reach
        // the ELF GNU tokenizer before lld ever sees the expanded response file.
        let response = TempFile::create_with_contents(
            "nested-reld-only-flags",
            b"--fork --thread-count 2 bar.o",
        );
        let argv = vec![
            OsString::from("ld.reld"),
            OsString::from("--icf=all"),
            OsString::from(format!("@{}", response.path().display())),
            OsString::from("foo.o"),
        ];
        let route = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap();
        assert_eq!(route.engine.name, "lld");
        let (forwarded, dropped) = forwarded_args_for_engine(argv, route.engine).unwrap();
        assert_eq!(
            forwarded,
            vec![
                OsString::from("--icf=all"),
                OsString::from("bar.o"),
                OsString::from("foo.o"),
            ]
        );
        assert!(dropped.iter().any(|flag| flag == "--fork"));
        assert!(dropped.iter().any(|flag| flag == "--thread-count 2"));

        // Every `NativeControl` name forces the native engine and conflicts loudly with a routed
        // requirement, rather than silently forwarding.
        for name in NATIVE_CONTROL_RELD_FLAGS {
            let argv = vec![
                OsString::from("ld.reld"),
                OsString::from(format!("--{name}")),
                OsString::from("--icf=all"),
            ];
            let error = select_route_with_env(&argv, BridgeTarget::Elf.into(), None).unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("No bundled ELF engine supports"),
                "flag {name}: unexpected message: {message}"
            );
        }

        // Completeness: every #123 D6 reld-only flag is classified exactly once, as `Stripped`
        // or `NativeControl`. (`--rpath-link` is excluded: `ld.lld` honors it, so it's forwarded
        // rather than stripped -- see the `STRIPPED_RELD_FLAGS` doc comment.)
        const INVENTORY: &[&str] = &[
            "debug-fuel",
            "discard-sframe",
            "fallocate-output-file",
            "no-fallocate-output-file",
            "fork",
            "no-fork",
            "gc-stats-ignore",
            "got-plt-syms",
            "madvise-huge-pages",
            "no-madvise-huge-pages",
            "nix-rpath",
            "no-identity-comment",
            "no-string-merge",
            "no-threads",
            "no-update-in-place",
            "update-in-place",
            "prepopulate-maps",
            "reld-experimental-sframe",
            "reld-experiments",
            "sym-info",
            "thread-count",
            "time",
            "verbose-gc-stats",
            "write-gc-stats",
            "validate-output",
            "write-layout",
            "write-trace",
            "mmap-output-file",
            "no-mmap-output-file",
            "threads",
        ];
        for name in INVENTORY {
            let in_stripped = STRIPPED_RELD_FLAGS
                .iter()
                .any(|(candidate, _)| candidate == name);
            let in_native_control = NATIVE_CONTROL_RELD_FLAGS.contains(name);
            assert!(
                in_stripped ^ in_native_control,
                "{name} must be in exactly one of STRIPPED_RELD_FLAGS or NATIVE_CONTROL_RELD_FLAGS"
            );
        }
    }

    // --- reld#184: TargetProbe-driven engine selection ------------------------------------

    /// Builds a 20-byte ELF header the way `target_probe`'s own tests do.
    fn elf_header(class: u8, data: u8, machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&object::elf::ELFMAG);
        bytes[4] = class;
        bytes[5] = data;
        bytes[6] = 1; // EI_VERSION
        let (e_type, e_machine) = if data == object::elf::ELFDATA2MSB.0 {
            (1_u16.to_be_bytes(), machine.to_be_bytes())
        } else {
            (1_u16.to_le_bytes(), machine.to_le_bytes())
        };
        bytes[16..18].copy_from_slice(&e_type);
        bytes[18..20].copy_from_slice(&e_machine);
        bytes
    }

    /// Builds a 20-byte COFF object header with the machine field at offset 0.
    fn coff_header(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..2].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn argv_with_prog(prog: &str, args: &[&str]) -> Vec<OsString> {
        std::iter::once(OsString::from(prog))
            .chain(args.iter().map(|arg| OsString::from(*arg)))
            .collect()
    }

    #[test]
    fn target_probe_picks_engine_before_format() {
        use object::elf::ELFCLASS32;
        use object::elf::ELFCLASS64;
        use object::elf::ELFDATA2LSB;
        use object::elf::ELFDATA2MSB;
        use object::elf::EM_ARM;
        use object::elf::EM_PPC64;

        let dir = tempfile::tempdir().unwrap();

        let argv_cases: &[(&[&str], &str, &str)] = &[
            (&["-m", "i386pep"], "lld-mingw", "target:pe-mingw"),
            (&["-m", "elf_i386"], "lld", "target:i386"),
            (
                &[
                    "-arch",
                    "arm64",
                    "-platform_version",
                    "macos",
                    "11.0",
                    "14.0",
                ],
                "ld64.lld",
                "target:mach-o",
            ),
            (&["/OUT:a.exe"], "lld-link", "target:coff"),
            (&["-m", "elf_x86_64", "x.o"], "reld", "default"),
        ];
        for &(args, engine, reason) in argv_cases {
            let target = resolve_link_target(ExplicitFormat::None, args, None, BridgeTarget::Elf);
            let argv = argv_with_prog("reld", args);
            let route = select_route_with_env(&argv, target, None).unwrap();
            assert_eq!(route.engine.name, engine, "args {args:?}");
            assert_eq!(route.reason.label(), reason, "args {args:?}");
        }

        let arm_elf = dir.path().join("arm.o");
        std::fs::write(&arm_elf, elf_header(ELFCLASS32.0, ELFDATA2LSB.0, EM_ARM.0)).unwrap();
        let ppc64_elf = dir.path().join("ppc64.o");
        std::fs::write(
            &ppc64_elf,
            elf_header(ELFCLASS64.0, ELFDATA2MSB.0, EM_PPC64.0),
        )
        .unwrap();
        let bitcode = dir.path().join("bitcode.o");
        std::fs::write(&bitcode, b"BC\xC0\xDE\0\0\0\0").unwrap();

        let input_cases: &[(&Path, &str, &str)] = &[
            (arm_elf.as_path(), "lld", "target:arm"),
            (ppc64_elf.as_path(), "lld", "target:ppc64-be"),
            (bitcode.as_path(), "lld", "input:bitcode"),
        ];
        for &(path, engine, reason) in input_cases {
            let path_str = path.to_str().unwrap();
            let args = [path_str];
            let target = resolve_link_target(ExplicitFormat::None, &args, None, BridgeTarget::Elf);
            let argv = argv_with_prog("reld", &args);
            let route = select_route_with_env(&argv, target, None).unwrap();
            assert_eq!(route.engine.name, engine, "path {}", path.display());
            assert_eq!(route.reason.label(), reason, "path {}", path.display());
        }

        let script = dir.path().join("script.ld");
        std::fs::write(
            &script,
            b"SECTIONS { .foo : { *(.foo) } } INSERT AFTER .text;",
        )
        .unwrap();
        let script_str = script.to_str().unwrap();
        let script_args = ["-T", script_str];
        let target =
            resolve_link_target(ExplicitFormat::None, &script_args, None, BridgeTarget::Elf);
        let argv = argv_with_prog("reld", &script_args);
        let route = select_route_with_env(&argv, target, None).unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason.label(), "input:linker-script-insert");
    }

    #[test]
    fn explicit_flavor_and_engine_override_beats_the_probe() {
        let mingw_args = ["-m", "i386pep"];
        let coff_target =
            resolve_link_target(ExplicitFormat::Coff, &mingw_args, None, BridgeTarget::Elf);
        assert_eq!(coff_target.format(), BridgeTarget::Coff);
        let argv = argv_with_prog("reld-link", &mingw_args);
        let route = select_route_with_env(&argv, coff_target, None).unwrap();
        assert_eq!(route.engine.name, "lld-link");

        let macho_args = ["-o", "a.out"];
        let macho_target =
            resolve_link_target(ExplicitFormat::MachO, &macho_args, None, BridgeTarget::Elf);
        assert_eq!(macho_target.format(), BridgeTarget::MachO);

        let gnu_mingw =
            resolve_link_target(ExplicitFormat::Gnu, &mingw_args, None, BridgeTarget::Elf);
        assert_eq!(gnu_mingw.format(), BridgeTarget::MinGw);

        let gnu_out =
            resolve_link_target(ExplicitFormat::Gnu, &["/OUT:x"], None, BridgeTarget::Elf);
        assert_eq!(gnu_out.format(), BridgeTarget::Elf);

        let override_flag = resolve_link_target(
            ExplicitFormat::None,
            &["--engine=lld", "-m", "i386pep"],
            None,
            BridgeTarget::Elf,
        );
        assert_eq!(override_flag.format(), BridgeTarget::Elf);
        let argv = argv_with_prog("ld.reld", &["--engine=lld", "-m", "i386pep"]);
        let route = select_route_with_env(&argv, override_flag, None).unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::OverrideFlag);

        let env_override = resolve_link_target(
            ExplicitFormat::None,
            &["-m", "elf_i386"],
            Some("lld-link"),
            BridgeTarget::Elf,
        );
        assert_eq!(env_override.format(), BridgeTarget::Coff);

        let conflicting_override = resolve_link_target(
            ExplicitFormat::None,
            &["--engine=reld", "-m", "elf_i386"],
            None,
            BridgeTarget::Elf,
        );
        let argv = argv_with_prog("ld.reld", &["--engine=reld", "-m", "elf_i386"]);
        let error = select_route_with_env(&argv, conflicting_override, None).unwrap_err();
        assert!(
            error.to_string().contains("target:i386"),
            "unexpected message: {error:?}"
        );

        let no_args: &[&str] = &[];
        let no_signal_coff =
            resolve_link_target(ExplicitFormat::None, no_args, None, BridgeTarget::Coff);
        assert_eq!(no_signal_coff.format(), BridgeTarget::Coff);
        let no_signal_elf =
            resolve_link_target(ExplicitFormat::None, no_args, None, BridgeTarget::Elf);
        assert_eq!(no_signal_elf.format(), BridgeTarget::Elf);
    }

    #[test]
    fn mingw_route_injects_emulation_only_when_absent() {
        // IMAGE_FILE_MACHINE_AMD64.
        const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;

        let dir = tempfile::tempdir().unwrap();
        let coff_input = dir.path().join("in.obj");
        std::fs::write(&coff_input, coff_header(IMAGE_FILE_MACHINE_AMD64)).unwrap();
        let coff_input_str = coff_input.to_str().unwrap();

        let args = ["--engine=lld-mingw", coff_input_str];
        let target = resolve_link_target(ExplicitFormat::Gnu, &args, None, BridgeTarget::Elf);
        assert_eq!(target.format(), BridgeTarget::MinGw);
        let argv = argv_with_prog("reld", &args);
        let route = select_route_with_env(&argv, target, None).unwrap();
        assert_eq!(route.engine.name, "lld-mingw");
        assert_eq!(route.mingw_emulation, Some("i386pep"));

        let args = ["--engine=lld-mingw", "-m", "i386pep", coff_input_str];
        let target = resolve_link_target(ExplicitFormat::Gnu, &args, None, BridgeTarget::Elf);
        let argv = argv_with_prog("reld", &args);
        let route = select_route_with_env(&argv, target, None).unwrap();
        assert_eq!(route.mingw_emulation, None);
    }

    #[test]
    fn probe_skips_option_values() {
        use object::elf::ELFCLASS32;
        use object::elf::ELFCLASS64;
        use object::elf::ELFDATA2LSB;
        use object::elf::EM_ARM;
        use object::elf::EM_X86_64;

        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out");
        std::fs::write(&out_path, elf_header(ELFCLASS32.0, ELFDATA2LSB.0, EM_ARM.0)).unwrap();
        let x86_path = dir.path().join("x86.o");
        std::fs::write(
            &x86_path,
            elf_header(ELFCLASS64.0, ELFDATA2LSB.0, EM_X86_64.0),
        )
        .unwrap();

        let out_str = out_path.to_str().unwrap();
        let x86_str = x86_path.to_str().unwrap();
        let args = ["-o", out_str, x86_str];
        let probe = probe_link_target(&args).unwrap();
        assert_eq!(probe.arch, Some(crate::target_probe::ProbedArch::X86_64));
    }

    // --- reld#192: Mach-O is keyed on the target, not the host --------------------------------

    /// A 32-byte thin Mach-O header: `MH_MAGIC_64`, little-endian, `CPU_TYPE_ARM64`.
    fn arm64_macho_header() -> Vec<u8> {
        let mut bytes = vec![0_u8; 32];
        bytes[0..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x0100_000c_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn macho_from_linux_routes_to_ld64_lld() {
        let dir = tempfile::tempdir().unwrap();
        let macho_input = dir.path().join("hello.o");
        std::fs::write(&macho_input, arm64_macho_header()).unwrap();
        let macho_input_str = macho_input.to_str().unwrap();

        let clang_ld64_args: &[&str] = &[
            "-demangle",
            "-dynamic",
            "-arch",
            "arm64",
            "-platform_version",
            "macos",
            "11.0.0",
            "0.0.0",
            "-o",
            "hello",
            "hello.o",
        ];
        let platform_version_args: &[&str] =
            &["-platform_version", "macos", "11.0", "14.0", "-o", "a"];
        let input_header_args: &[&str] = &[macho_input_str];

        for args in [clang_ld64_args, platform_version_args, input_header_args] {
            let target = resolve_link_target(ExplicitFormat::None, args, None, BridgeTarget::Elf);
            let argv = argv_with_prog("reld", args);
            let route = select_route_with_env(&argv, target, None).unwrap();
            assert_eq!(route.engine.name, "ld64.lld", "args {args:?}");
            assert_eq!(route.reason.label(), "target:mach-o", "args {args:?}");
        }

        // On a Mach-O host, the same clang argv agrees with the host default, so the reason is
        // `default` rather than `target:mach-o`.
        let target = resolve_link_target(
            ExplicitFormat::None,
            clang_ld64_args,
            None,
            BridgeTarget::MachO,
        );
        let argv = argv_with_prog("reld", clang_ld64_args);
        let route = select_route_with_env(&argv, target, None).unwrap();
        assert_eq!(route.engine.name, "ld64.lld");
        assert_eq!(route.reason.label(), "default");
    }

    #[test]
    fn darwin_cc_argv_fails_naming_linker_flavor() {
        let flag_cases: &[&[&str]] = &[
            &["-nodefaultlibs"],
            &["-Wl,-dead_strip"],
            &["-mmacosx-version-min=11.0"],
            &["-dynamiclib"],
            &["-isysroot", "/sdk"],
        ];

        for flag_args in flag_cases {
            let mut args: Vec<&str> = vec!["-arch", "arm64"];
            args.extend_from_slice(flag_args);
            args.extend_from_slice(&["-o", "a", "a.o"]);

            let target = resolve_link_target(ExplicitFormat::None, &args, None, BridgeTarget::Elf);
            let argv = argv_with_prog("reld", &args);
            let error = select_route_with_env(&argv, target, None).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("darwin-cc"), "args {args:?}: {message}");
            assert!(
                message.contains(&format!("`{}`", flag_args[0])),
                "args {args:?}: {message}"
            );
            assert!(
                message.contains("-Clinker-flavor=ld64.lld"),
                "args {args:?}: {message}"
            );
        }

        // No `-arch` at all: the `DriverTarget` signal (`--target=`) alone must still route to
        // Mach-O and trip the darwin-cc rule.
        let args = ["--target=arm64-apple-macos11", "-nodefaultlibs"];
        let target = resolve_link_target(ExplicitFormat::None, &args, None, BridgeTarget::Elf);
        assert_eq!(target.format(), BridgeTarget::MachO);
        let argv = argv_with_prog("reld", &args);
        let error = select_route_with_env(&argv, target, None).unwrap_err();
        let message = error.to_string();
        // The rejection names the first darwin-cc flag in argv order; `--target=` is itself a
        // clang-driver argument, so it is the one reported here.
        assert!(
            message.contains("`--target=arm64-apple-macos11`")
                || message.contains("`-nodefaultlibs`"),
            "{message}"
        );
        assert!(message.contains("-Clinker-flavor=ld64.lld"), "{message}");

        // An explicit `-flavor darwin` (`ExplicitFormat::MachO`) argv with only ld64 flags routes
        // fine.
        let ld64_only: &[&str] = &[
            "-arch",
            "arm64",
            "-platform_version",
            "macos",
            "11.0",
            "14.0",
            "-lSystem",
            "-o",
            "a",
            "a.o",
        ];
        let target = resolve_link_target(ExplicitFormat::MachO, ld64_only, None, BridgeTarget::Elf);
        let argv = argv_with_prog("reld", ld64_only);
        let route = select_route_with_env(&argv, target, None).unwrap();
        assert_eq!(route.engine.name, "ld64.lld");

        // An ELF argv containing `-Wl,foo` is NOT rejected by this rule.
        let elf_args = ["-Wl,foo", "-o", "a", "a.o"];
        let target = resolve_link_target(ExplicitFormat::None, &elf_args, None, BridgeTarget::Elf);
        assert_eq!(target.format(), BridgeTarget::Elf);
        let argv = argv_with_prog("reld", &elf_args);
        select_route_with_env(&argv, target, None).unwrap();
    }
}
