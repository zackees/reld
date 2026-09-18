//! Behavior of a bridged link that the unit tests cannot reach, because it depends on how a real
//! child process terminates (reld#123 Phase 0, D6).
//!
//! reld#184 extends this file with end-to-end routing tests: they run the built `reld` binary
//! against a fake bridge linker that records its argv, and assert that the `RELD_LOG_ENGINE`
//! routing line on stderr names the engine `TargetProbe` picked.
//!
//! reld#192 adds Mach-O-from-any-host routing and the darwin-cc rule.

use reld_core::platforms::host;
use reld_core::platforms::host::HostOs;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn reld() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_reld"))
}

/// Whether this host has a POSIX shell to run the fake `/bin/sh` linker scripts these tests write.
fn posix_host() -> bool {
    matches!(
        host::os(),
        HostOs::Linux | HostOs::Android | HostOs::MacOs | HostOs::FreeBsd | HostOs::Illumos
    )
}

/// A crashed bridge must be diagnosable rather than collapsing to a bare exit 1: reld reports
/// `128 + signal` and names the engine and the signal on stderr.
#[test]
fn bridged_child_signal_is_reported() {
    if !matches!(
        host::os(),
        HostOs::Linux | HostOs::Android | HostOs::MacOs | HostOs::FreeBsd | HostOs::Illumos
    ) {
        eprintln!("skipping: host has no POSIX signals");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let linker = directory.path().join("crashing-linker.sh");
    std::fs::write(&linker, "#!/bin/sh\nkill -SEGV $$\n").expect("could not write fake linker");
    reld_core::platforms::fs::make_executable(
        &std::fs::File::open(&linker).expect("could not open fake linker"),
    )
    .expect("could not make the fake linker executable");

    let output = Command::new(reld())
        .arg("--engine=lld")
        .arg("-o")
        .arg(directory.path().join("out"))
        .arg(directory.path().join("missing-input.o"))
        .env("RELD_BRIDGE_LINKER", &linker)
        .output()
        .expect("failed to run reld");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // SIGSEGV is 11 on every platform reld bridges on.
    assert_eq!(output.status.code(), Some(139), "stderr: {stderr}");
    assert!(
        stderr.contains("lld bridge terminated by signal 11"),
        "stderr: {stderr}"
    );
}

// --- End-to-end routing (reld#184): TargetProbe picks the engine ---------------------------

/// Writes a fake linker that records every argument it is given, one per line, to
/// `$RELD_TEST_RECORD` and exits successfully. Returns the script path and the path the recorded
/// argv is written to.
fn recording_linker(dir: &Path) -> (PathBuf, PathBuf) {
    let script = dir.join("recording-linker.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RELD_TEST_RECORD\"\n",
    )
    .expect("could not write fake linker");
    reld_core::platforms::fs::make_executable(
        &std::fs::File::open(&script).expect("could not open fake linker"),
    )
    .expect("could not make the fake linker executable");
    (script, dir.join("argv.txt"))
}

/// Runs `reld` with `args`, routing any bridge to the fake recording linker and enabling the
/// `RELD_LOG_ENGINE` routing note. Returns the process output and the recorded argv lines (empty
/// when the bridge never ran).
fn run_routed(args: &[OsString], dir: &Path) -> (std::process::Output, Vec<String>) {
    let (linker, record) = recording_linker(dir);
    let output = Command::new(reld())
        .args(args)
        .env(reld_core::bridge::RELD_LOG_ENGINE_ENV, "1")
        .env(reld_core::bridge::RELD_BRIDGE_LINKER_ENV, &linker)
        .env("RELD_TEST_RECORD", &record)
        .env_remove(reld_core::bridge::RELD_ENGINE_ENV)
        .output()
        .expect("failed to run reld");

    let argv = match std::fs::read_to_string(&record) {
        Ok(contents) => contents.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    };
    (output, argv)
}

/// Whether `lines` contains `first` immediately followed by `second`.
fn has_adjacent_lines(lines: &[String], first: &str, second: &str) -> bool {
    lines
        .windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

/// Builds a fake 64-byte ELF header: `class`/`data`/`machine` are `EI_CLASS`/`EI_DATA`/
/// `e_machine`, e.g. `elf_header(1, 1, 40)` is an ELFCLASS32 LSB EM_ARM (40) object.
fn elf_header(class: u8, data: u8, machine: u16) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[0] = 0x7f;
    bytes[1] = b'E';
    bytes[2] = b'L';
    bytes[3] = b'F';
    bytes[4] = class;
    bytes[5] = data;
    bytes[6] = 1; // EI_VERSION
    let big_endian = data == 2; // ELFDATA2MSB
    let e_type: [u8; 2] = if big_endian {
        1_u16.to_be_bytes()
    } else {
        1_u16.to_le_bytes()
    };
    let e_machine: [u8; 2] = if big_endian {
        machine.to_be_bytes()
    } else {
        machine.to_le_bytes()
    };
    bytes[16..18].copy_from_slice(&e_type);
    bytes[18..20].copy_from_slice(&e_machine);
    bytes
}

/// Builds a fake 20-byte COFF header: `machine` is `IMAGE_FILE_HEADER.Machine`, little-endian at
/// offset 0, e.g. `coff_header(0x8664)` is an `IMAGE_FILE_MACHINE_AMD64` object.
fn coff_header(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0_u8; 20];
    bytes[0..2].copy_from_slice(&machine.to_le_bytes());
    bytes
}

/// Builds a fake 32-byte Mach-O header: the 64-bit magic `0xfeedfacf` at offset 0 and
/// `CPU_TYPE_ARM64` (`0x0100000c`) at offset 4, both little-endian, e.g. `macho_header()` is an
/// `MH_MAGIC_64` ARM64 object.
fn macho_header() -> Vec<u8> {
    let mut bytes = vec![0_u8; 32];
    bytes[0..4].copy_from_slice(&0xfeedfacf_u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100000c_u32.to_le_bytes());
    bytes
}

/// A GNU-style `-m i386pep` emulation, with no matching input, must route to the MinGW driver
/// rather than the plain COFF `lld-link` driver: MinGW links use GNU argv conventions (`-o`, not
/// `/OUT:`), which `lld-link` cannot parse.
#[test]
fn mingw_emulation_routes_to_lld_mingw() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "-m".into(),
        "i386pep".into(),
        "-o".into(),
        directory.path().join("a.exe").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld-mingw (bridge, reason=target:pe-mingw)"),
        "stderr: {stderr}"
    );
    assert!(
        has_adjacent_lines(&argv, "-m", "i386pep"),
        "recorded argv: {argv:?}"
    );
    assert!(
        !argv.iter().any(|line| line == "-flavor"),
        "recorded argv: {argv:?}"
    );
}

/// `-m elf_i386` asks for an architecture reld's native ELF engine doesn't implement, so the
/// default routing must fall back to the ELF `lld` driver.
#[test]
fn foreign_elf_emulation_routes_to_lld() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "-m".into(),
        "elf_i386".into(),
        "-o".into(),
        directory.path().join("a").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld (bridge, reason=target:i386)"),
        "stderr: {stderr}"
    );
}

/// With no `-m` at all, an EM_ARM input header is itself a signal that this is not a link reld's
/// native engine can perform, so it must route to `lld`.
#[test]
fn foreign_elf_input_routes_to_lld() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let input = directory.path().join("arm.o");
    std::fs::write(&input, elf_header(1, 1, 40)).expect("could not write fake ARM object");

    let args: Vec<OsString> = vec!["-o".into(), directory.path().join("a").into(), input.into()];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld (bridge, reason=target:arm)"),
        "stderr: {stderr}"
    );
}

/// A GNU-style link (`-o`, not `/OUT:`) with a COFF AMD64 input and no explicit `-m` is a MinGW
/// link that forgot to say so; reld must inject `-m i386pep` into the forwarded argv so the
/// bridged driver knows the emulation.
#[test]
fn coff_input_without_emulation_gets_i386pep() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let input = directory.path().join("x.obj");
    std::fs::write(&input, coff_header(0x8664)).expect("could not write fake COFF object");

    let args: Vec<OsString> = vec![
        "-o".into(),
        directory.path().join("a.exe").into(),
        input.into(),
    ];
    let (output, argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld-mingw (bridge, reason=target:pe-mingw)"),
        "stderr: {stderr}"
    );
    assert!(
        has_adjacent_lines(&argv, "-m", "i386pep"),
        "recorded argv: {argv:?}"
    );
}

/// `-arch arm64` and `/OUT:` are unambiguous format signals for Mach-O and COFF respectively, and
/// must keep routing to `ld64.lld`/`lld-link` exactly as before TargetProbe was wired in.
#[test]
fn macho_and_msvc_signals_pick_their_engines() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "-arch".into(),
        "arm64".into(),
        "-o".into(),
        directory.path().join("a").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=ld64.lld (bridge, reason="),
        "stderr: {stderr}"
    );

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let mut out_flag = OsString::from("/OUT:");
    out_flag.push(directory.path().join("a.exe"));
    let args: Vec<OsString> = vec![out_flag, directory.path().join("missing.obj").into()];
    let (output, _argv) = run_routed(&args, directory.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld-link (bridge, reason="),
        "stderr: {stderr}"
    );
}

/// A `-T` script with an `INSERT AFTER .text` command needs GNU linker-script semantics reld's
/// native engine doesn't implement, so it must route to `lld` even for an ordinary ELF emulation.
#[test]
fn insert_script_routes_to_lld() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let script = directory.path().join("insert.ld");
    std::fs::write(
        &script,
        "SECTIONS { .foo : { *(.foo) } } INSERT AFTER .text;\n",
    )
    .expect("could not write linker script");

    let args: Vec<OsString> = vec![
        "-m".into(),
        "elf_x86_64".into(),
        "-T".into(),
        script.into(),
        "-o".into(),
        directory.path().join("a").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld (bridge, reason=input:linker-script-insert)"),
        "stderr: {stderr}"
    );
}

/// An explicit `-flavor <name>` (reld's own multi-call dispatch) and an explicit `--engine=`
/// override both beat whatever TargetProbe would have picked on its own; an override that asks
/// for an engine the target can't support is still a hard error naming the TargetProbe reason.
#[test]
fn explicit_flavor_and_engine_beat_probe() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let mut out_flag = OsString::from("/OUT:");
    out_flag.push(directory.path().join("a.exe"));
    let args: Vec<OsString> = vec![
        "-flavor".into(),
        "link".into(),
        "-m".into(),
        "i386pep".into(),
        out_flag,
    ];
    let (output, _argv) = run_routed(&args, directory.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld-link (bridge, reason=default)"),
        "stderr: {stderr}"
    );

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "--engine=lld".into(),
        "-m".into(),
        "i386pep".into(),
        "-o".into(),
        directory.path().join("a").into(),
    ];
    let (output, argv) = run_routed(&args, directory.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=lld (bridge, reason=override(--engine))"),
        "stderr: {stderr}"
    );
    assert!(
        !argv.iter().any(|line| line == "--engine=lld"),
        "argv: {argv:?}"
    );

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let record = directory.path().join("argv.txt");
    let args: Vec<OsString> = vec![
        "--engine=reld".into(),
        "-m".into(),
        "elf_i386".into(),
        "-o".into(),
        directory.path().join("a").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("target:i386"), "stderr: {stderr}");
    assert!(!record.exists(), "stderr: {stderr}");
}

/// A plain ELF link with no capability the native engine lacks must keep routing natively, exactly
/// as before TargetProbe was wired in: the bridge (and its recording linker) must never run.
#[test]
fn native_elf_route_is_unchanged() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let record = directory.path().join("argv.txt");
    let args: Vec<OsString> = vec!["-m".into(), "elf_x86_64".into(), "--version".into()];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    // On an ELF host the probe agrees with the host default; on macOS the probe is what moved
    // the format to ELF, so the reason names the target instead.
    let expected = if host::os() == HostOs::MacOs {
        "reld: engine=reld (native, reason=target:elf)"
    } else {
        "reld: engine=reld (native, reason=default)"
    };
    assert!(stderr.contains(expected), "stderr: {stderr}");
    assert!(!record.exists(), "stderr: {stderr}");
}

// --- Mach-O from any host and the darwin-cc rule (reld#192) ---------------------------------

/// Clang's Darwin driver invokes the linker with `-demangle -dynamic -arch <arch>
/// -platform_version <platform> <min> <sdk> ...`; when `--ld-path=reld` puts reld in that seat, it
/// must route to `ld64.lld` on every host, not only on macOS where the host default already
/// happens to be Mach-O.
#[test]
fn macho_from_linux_links() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "-demangle".into(),
        "-dynamic".into(),
        "-arch".into(),
        "arm64".into(),
        "-platform_version".into(),
        "macos".into(),
        "11.0.0".into(),
        "0.0.0".into(),
        "-o".into(),
        directory.path().join("hello").into(),
        directory.path().join("missing.o").into(),
    ];
    let (output, argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reld: engine=ld64.lld (bridge, reason="),
        "stderr: {stderr}"
    );
    let expected = if host::os() == HostOs::MacOs {
        "reld: engine=ld64.lld (bridge, reason=default)"
    } else {
        "reld: engine=ld64.lld (bridge, reason=target:mach-o)"
    };
    assert!(stderr.contains(expected), "stderr: {stderr}");
    assert!(
        has_adjacent_lines(&argv, "-arch", "arm64"),
        "recorded argv: {argv:?}"
    );
    assert!(
        has_adjacent_lines(&argv, "-platform_version", "macos"),
        "recorded argv: {argv:?}"
    );
    assert!(
        !argv.iter().any(|line| line.starts_with("--engine=")),
        "recorded argv: {argv:?}"
    );
}

/// With no `-arch` at all, a Mach-O input header is itself a signal that this is a Mach-O link,
/// exactly as an ELF or COFF input header is for those formats, so it must route to `ld64.lld` on
/// every host.
#[test]
fn macho_input_header_routes_to_ld64_lld() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let input = directory.path().join("hello.o");
    std::fs::write(&input, macho_header()).expect("could not write fake Mach-O object");

    let args: Vec<OsString> = vec!["-o".into(), directory.path().join("a").into(), input.into()];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = if host::os() == HostOs::MacOs {
        "reld: engine=ld64.lld (bridge, reason=default)"
    } else {
        "reld: engine=ld64.lld (bridge, reason=target:mach-o)"
    };
    assert!(stderr.contains(expected), "stderr: {stderr}");
}

/// rustc's default linker flavor for an Apple target wraps the linker in the platform `cc`, which
/// emits clang-driver-only flags (`-nodefaultlibs`, `-mmacosx-version-min=`, `-Wl,`, ...) that
/// `ld64.lld` cannot parse; a Mach-O-routed argv carrying one of those flags must fail before the
/// bridge ever runs, naming `-Clinker-flavor=ld64.lld` as the fix. The same link expressed the way
/// `-Clinker-flavor=ld64.lld` calls the linker directly (`-flavor darwin ...`, no cc-only flags)
/// must still link.
#[test]
fn macos_rustc_default_flavor_links() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let record = directory.path().join("argv.txt");
    let args: Vec<OsString> = vec![
        "-arch".into(),
        "arm64".into(),
        "-mmacosx-version-min=11.0.0".into(),
        directory.path().join("main.o").into(),
        "-nodefaultlibs".into(),
        "-lSystem".into(),
        "-Wl,-dead_strip".into(),
        "-o".into(),
        directory.path().join("app").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("-Clinker-flavor=ld64.lld"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("`-mmacosx-version-min=")
            || stderr.contains("`-nodefaultlibs`")
            || stderr.contains("`-Wl,`"),
        "stderr: {stderr}"
    );
    assert!(!record.exists(), "stderr: {stderr}");

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let args: Vec<OsString> = vec![
        "-flavor".into(),
        "darwin".into(),
        "-arch".into(),
        "arm64".into(),
        "-platform_version".into(),
        "macos".into(),
        "11.0.0".into(),
        "14.0".into(),
        directory.path().join("main.o").into(),
        "-lSystem".into(),
        "-dead_strip".into(),
        "-o".into(),
        directory.path().join("app").into(),
    ];
    let (output, argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("reld: engine=ld64.lld (bridge, reason="),
        "stderr: {stderr}"
    );
    assert!(
        argv.iter().any(|line| line == "-dead_strip"),
        "recorded argv: {argv:?}"
    );
    assert!(
        !argv.iter().any(|line| line.starts_with("-Wl,")),
        "recorded argv: {argv:?}"
    );
}

/// `--target=<apple triple>` is as unambiguous a Mach-O signal as `-arch`, but it is also a
/// clang-driver-only flag under the darwin-cc rule: a Mach-O-routed argv that carries it without
/// an `-arch` must still fail before the bridge runs.
#[test]
fn darwin_cc_driver_target_is_rejected() {
    if !posix_host() {
        eprintln!("skipping: host has no POSIX shell scripts");
        return;
    }

    let directory = tempfile::tempdir().expect("could not create temp dir");
    let record = directory.path().join("argv.txt");
    let args: Vec<OsString> = vec![
        "--target=arm64-apple-macos11".into(),
        "-nodefaultlibs".into(),
        "-o".into(),
        directory.path().join("a").into(),
        directory.path().join("a.o").into(),
    ];
    let (output, _argv) = run_routed(&args, directory.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("-Clinker-flavor=ld64.lld"),
        "stderr: {stderr}"
    );
    assert!(!record.exists(), "stderr: {stderr}");
}
