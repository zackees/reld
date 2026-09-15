//! End-to-end test for reld's NixOS RUNPATH derivation, mirroring nixpkgs' `ld-wrapper.sh`.
//!
//! These tests link a real shared object against a library in a fake Nix store directory and
//! assert that reld derives the matching `DT_RUNPATH`. They are skipped when no C compiler is
//! available, and only run on Linux, where `cc` produces ELF objects.
#![cfg(target_os = "linux")]

use object::Object as _;
use object::ObjectSection as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn reld() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_reld"))
}

fn find_cc() -> Option<&'static str> {
    ["cc", "gcc", "clang"]
        .into_iter()
        .find(|cc| Command::new(cc).arg("--version").output().is_ok())
}

/// Builds `lib/libfoo.so` and `main.o` in a fresh temp dir, returning the guard, the store path
/// string and the lib directory string.
fn build_fixture() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("could not create temp dir");
    let lib_dir = dir.path().join("lib");
    std::fs::create_dir_all(&lib_dir).expect("could not create lib dir");
    std::fs::write(dir.path().join("foo.c"), "int foo(void) { return 42; }\n").unwrap();
    std::fs::write(
        dir.path().join("main.c"),
        "int foo(void);\nint call_foo(void) { return foo() + 1; }\n",
    )
    .unwrap();

    let cc = find_cc().expect("no C compiler available");
    let status = Command::new(cc)
        .args(["-shared", "-fPIC", "-o"])
        .arg(lib_dir.join("libfoo.so"))
        .arg(dir.path().join("foo.c"))
        .status()
        .expect("failed to run cc");
    assert!(status.success(), "cc failed to build libfoo.so");
    let status = Command::new(cc)
        .args(["-c", "-fPIC", "-o"])
        .arg(dir.path().join("main.o"))
        .arg(dir.path().join("main.c"))
        .status()
        .expect("failed to run cc");
    assert!(status.success(), "cc failed to build main.o");

    let store = dir.path().to_string_lossy().into_owned();
    let lib = lib_dir.to_string_lossy().into_owned();
    (dir, store, lib)
}

/// Links `main.o` into a shared object against `-lfoo` with the given store and environment, and
/// returns the linked output path.
fn link(fixture: &Path, out: &Path, store: &str, env: &[(&str, &str)]) {
    let mut cmd = Command::new(reld());
    cmd.args(["-shared", "-o"])
        .arg(out)
        .arg(fixture.join("main.o"))
        .arg("-L")
        .arg(fixture.join("lib"))
        .arg("-lfoo")
        .env("NIX_STORE", store);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("failed to run reld");
    assert!(
        output.status.success(),
        "reld failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Reads the `DT_RUNPATH`/`DT_RPATH` string from a linked ELF binary, if present.
fn read_runpath(bin: &Path) -> Option<String> {
    let data = std::fs::read(bin).ok()?;
    let file = object::read::elf::ElfFile64::<object::LittleEndian>::parse(data.as_slice()).ok()?;
    let endian = file.endian();
    let dynamic = file.section_by_name(".dynamic")?;
    let dynstr = file.section_by_name(".dynstr")?;
    let dyn_data = dynamic.data().ok()?;
    let str_data = dynstr.data().ok()?;
    let entries = object::pod::slice_from_all_bytes::<object::elf::Dyn64<_>>(dyn_data).ok()?;
    for entry in entries {
        let tag = entry.d_tag.get(endian);
        if tag == object::elf::DT_RUNPATH || tag == object::elf::DT_RPATH {
            let offset = entry.d_val.get(endian) as usize;
            if offset >= str_data.len() {
                return None;
            }
            let bytes = &str_data[offset..];
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            return Some(String::from_utf8_lossy(&bytes[..end]).into_owned());
        }
        if tag == object::elf::DT_NULL {
            break;
        }
    }
    None
}

#[test]
fn store_lib_dir_yields_runpath() {
    let Some(_cc) = find_cc() else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let (_dir, store, lib) = build_fixture();
    let out = PathBuf::from(format!("{store}/out.so"));
    link(Path::new(&store), &out, &store, &[]);
    assert_eq!(read_runpath(&out).as_deref(), Some(lib.as_str()));
}

#[test]
fn dont_set_rpath_disables_derivation() {
    let Some(_cc) = find_cc() else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let (_dir, store, _lib) = build_fixture();
    let out = PathBuf::from(format!("{store}/out.so"));
    link(
        Path::new(&store),
        &out,
        &store,
        &[("NIX_DONT_SET_RPATH", "1")],
    );
    assert_eq!(read_runpath(&out), None);
}

#[test]
fn non_store_dir_yields_no_runpath() {
    let Some(_cc) = find_cc() else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let (_dir, store, _lib) = build_fixture();
    let out = PathBuf::from(format!("{store}/out.so"));
    // The fixture's lib dir is not under the real store, so nothing is derived.
    link(Path::new(&store), &out, "/nix/store", &[]);
    assert_eq!(read_runpath(&out), None);
}
