//! Behavior of a bridged link that the unit tests cannot reach, because it depends on how a real
//! child process terminates (reld#123 Phase 0, D6).

use reld_core::platforms::host;
use reld_core::platforms::host::HostOs;
use std::path::Path;
use std::process::Command;

fn reld() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_reld"))
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
