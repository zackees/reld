use crate::arch::ArchKind;
use anyhow::Context;
use anyhow::Result;
use itertools::Itertools;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use tempfile::NamedTempFile;

fn objdump_config(arch: ArchKind) -> (&'static str, [&'static str; 2]) {
    match arch {
        ArchKind::Aarch64 => ("aarch64", ["aarch64-linux-gnu-objdump", "objdump"]),
        ArchKind::RiscV64 => ("riscv:rv64", ["riscv64-linux-gnu-objdump", "objdump"]),
        ArchKind::X86_64 => todo!(), // x86_64 objdump is not used in linker-diff currently
        ArchKind::LoongArch64 => ("Loongarch64", ["loongarch64-linux-gnu-objdump", "objdump"]),
        ArchKind::Ppc64 => (
            "powerpc:common64",
            ["powerpc64le-linux-gnu-objdump", "objdump"],
        ),
    }
}

fn find_objdump(arch: ArchKind) -> Option<&'static str> {
    let (_, candidates) = objdump_config(arch);
    candidates.into_iter().find(|bin| which::which(bin).is_ok())
}

pub fn decode_insn_with_objdump(insn: &[u8], address: u64, arch: ArchKind) -> Result<String> {
    // TODO: seems objdump cannot read from stdin
    let mut tmpfile = NamedTempFile::new()?;
    tmpfile.write_all(insn)?;
    tmpfile.flush()?;

    let (objdump_arch, _) = objdump_config(arch);
    let objdump = find_objdump(arch).unwrap();

    let command = Command::new(objdump)
        .arg("-b")
        .arg("binary")
        // Every target we support is little-endian. Be explicit, since some machines (e.g. the
        // generic `powerpc:common64`) default to big-endian and would otherwise disassemble the
        // raw bytes as garbage on a big-endian-defaulting host.
        .arg("--endian=little")
        .arg(format!("--adjust-vma=0x{address:x}"))
        .arg("-m")
        .arg(objdump_arch)
        .arg("-D")
        .arg(tmpfile.path())
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn objdump")?;

    let output = command.wait_with_output().expect("Failed to read stdout");
    let insn_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .context("No objdump output")?
        .to_owned();

    Ok(insn_line
        .split_whitespace()
        .skip(2)
        .join(" ")
        .replacen(' ', "\t", 1))
}

/// Whether the objdump `decode_insn_with_objdump` would run for `arch` is GNU binutils objdump,
/// the only one whose disassembly text these expectations match.
#[cfg(test)]
fn gnu_objdump_available(arch: ArchKind) -> bool {
    let Some(objdump) = find_objdump(arch) else {
        eprintln!("skipping {arch:?} objdump check: no objdump found on PATH");
        return false;
    };
    match Command::new(objdump).arg("--version").output() {
        Ok(output) if String::from_utf8_lossy(&output.stdout).contains("GNU objdump") => true,
        Ok(_) => {
            eprintln!("skipping {arch:?} objdump check: `{objdump}` is not GNU objdump");
            false
        }
        Err(error) => {
            eprintln!("skipping {arch:?} objdump check: `{objdump} --version` failed: {error}");
            false
        }
    }
}

#[test]
fn test_align_up() {
    // Some distributions don't enable the features in objdump required for disassembly of aarch64,
    // so we only check that we can disassemble if we're running on aarch64 or if test
    // cross-compilation is enabled.
    if (cfg!(target_arch = "aarch64")
        || std::env::var("RELD_TEST_CROSS")
            .is_ok_and(|v| v == "all" || v.split(',').any(|a| a == "aarch64")))
        && gnu_objdump_available(ArchKind::Aarch64)
    {
        assert_eq!(
            decode_insn_with_objdump(&[0xe3, 0x93, 0x44, 0xa9], 0x1000, ArchKind::Aarch64).unwrap(),
            "ldp\tx3, x4, [sp, #72]"
        );
    }

    if (cfg!(target_arch = "riscv64")
        || std::env::var("RELD_TEST_CROSS")
            .is_ok_and(|v| v == "all" || v.split(',').any(|a| a == "riscv64")))
        && gnu_objdump_available(ArchKind::RiscV64)
    {
        assert_eq!(
            decode_insn_with_objdump(&[0x00, 0x20, 0xb0, 0x23], 0x1000, ArchKind::RiscV64).unwrap(),
            "fld\tfa2,64(a5)"
        );
    }
}
