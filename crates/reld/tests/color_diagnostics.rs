//! End-to-end color behavior of native diagnostics (reld#144).
//!
//! Every case here runs the real binary with stderr captured through a pipe, which is the
//! "not a terminal" case: automatic detection must stay quiet, and only an explicit request (or
//! `CLICOLOR_FORCE`) may produce escape sequences. Terminal-attached behavior needs a pty and is
//! covered by the unit tests over the decision table in `reld_core::diagnostic`.

use std::path::Path;
use std::process::Command;
use std::process::Output;

const ESCAPE: char = '\x1b';
const RED_ERROR: &str = "\x1b[0;31merror: \x1b[0m";
const MAGENTA_WARNING: &str = "\x1b[0;35mwarning: \x1b[0m";

fn reld() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_reld"))
}

/// Runs reld with a missing input, which fails early and needs no toolchain.
fn run(arguments: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(reld());
    command.args(arguments).arg("definitely-missing-input.o");
    // A stale value in the ambient environment must not decide the outcome.
    command.env_remove("NO_COLOR");
    command.env_remove("CLICOLOR_FORCE");
    for (key, value) in environment {
        command.env(key, value);
    }
    command.output().expect("failed to run reld")
}

fn stderr_of(arguments: &[&str], environment: &[(&str, &str)]) -> String {
    let output = run(arguments, environment);
    assert!(!output.status.success(), "the link was expected to fail");
    String::from_utf8(output.stderr).expect("reld wrote non-UTF-8 diagnostics")
}

#[test]
fn piped_stderr_is_plain_by_default() {
    let stderr = stderr_of(&[], &[]);
    assert!(stderr.starts_with("reld: error: "), "{stderr:?}");
    assert!(!stderr.contains(ESCAPE), "{stderr:?}");
}

#[test]
fn color_can_be_forced_onto_a_pipe() {
    for arguments in [
        vec!["--color-diagnostics"],
        vec!["--color-diagnostics=always"],
        vec!["-color-diagnostics=always"],
    ] {
        let stderr = stderr_of(&arguments, &[]);
        assert!(
            stderr.starts_with(&format!("reld: {RED_ERROR}")),
            "{stderr:?}"
        );
    }
}

#[test]
fn explicitly_disabled_color_writes_no_escapes() {
    for arguments in [
        vec!["--no-color-diagnostics"],
        vec!["--color-diagnostics=never"],
        // The last flag wins, exactly as in ld.lld.
        vec!["--color-diagnostics=always", "--no-color-diagnostics"],
    ] {
        let stderr = stderr_of(&arguments, &[]);
        assert!(!stderr.contains(ESCAPE), "{arguments:?}: {stderr:?}");
    }
}

#[test]
fn auto_honors_no_color_and_clicolor_force() {
    let forced = stderr_of(&[], &[("CLICOLOR_FORCE", "1")]);
    assert!(forced.contains(RED_ERROR), "{forced:?}");

    // An explicit flag outranks the environment in both directions.
    let flag_wins = stderr_of(&["--no-color-diagnostics"], &[("CLICOLOR_FORCE", "1")]);
    assert!(!flag_wins.contains(ESCAPE), "{flag_wins:?}");

    let suppressed = stderr_of(&[], &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "0")]);
    assert!(!suppressed.contains(ESCAPE), "{suppressed:?}");
}

#[test]
fn warnings_use_the_lld_warning_color() {
    let stderr = stderr_of(&["--color-diagnostics=always", "-z", "unknownzopt"], &[]);
    assert!(stderr.contains(MAGENTA_WARNING), "{stderr:?}");
}

/// The flag is applied by a pre-scan of argv, so a diagnostic raised while parsing a *later*
/// argument is still colored as asked.
#[test]
fn argument_errors_honor_the_color_flag() {
    let output = Command::new(reld())
        .args(["--color-diagnostics=always", "--definitely-not-a-flag"])
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("failed to run reld");
    let stderr = String::from_utf8(output.stderr).expect("reld wrote non-UTF-8 diagnostics");
    assert!(!output.status.success());
    assert!(stderr.contains(RED_ERROR), "{stderr:?}");

    let output = Command::new(reld())
        .args(["--no-color-diagnostics", "--definitely-not-a-flag"])
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .expect("failed to run reld");
    let stderr = String::from_utf8(output.stderr).expect("reld wrote non-UTF-8 diagnostics");
    assert!(!output.status.success());
    assert!(!stderr.contains(ESCAPE), "{stderr:?}");
}
