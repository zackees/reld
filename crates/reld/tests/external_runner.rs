#![cfg(unix)]

mod external_process;

use external_process::TimedOutput;
use external_process::output_with_timeout;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[test]
fn completed_command_captures_output() {
    let mut command = Command::new("bash");
    command.arg("-c").arg("printf stdout; printf stderr >&2");

    let TimedOutput::Completed(output) =
        output_with_timeout(&mut command, Duration::from_secs(5)).unwrap()
    else {
        panic!("short command unexpectedly timed out");
    };

    assert!(output.status.success());
    assert_eq!(output.stdout, b"stdout");
    assert_eq!(output.stderr, b"stderr");
}

#[test]
fn timeout_kills_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let survivor_marker = temp.path().join("descendant-survived");
    let script = format!(
        "(sleep 1; printf survived > '{}') & wait",
        survivor_marker.display()
    );
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    let start = Instant::now();

    assert!(matches!(
        output_with_timeout(&mut command, Duration::from_millis(100)).unwrap(),
        TimedOutput::TimedOut
    ));
    assert!(start.elapsed() < Duration::from_secs(2));

    thread::sleep(Duration::from_millis(1_100));
    assert!(!survivor_marker.exists(), "descendant survived timeout");
    assert!(fs::read_dir(temp.path()).unwrap().next().is_none());
}

#[test]
fn completed_shell_does_not_leave_pipe_holding_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let survivor_marker = temp.path().join("descendant-survived");
    let script = format!(
        "(sleep 1; printf survived > '{}') &",
        survivor_marker.display()
    );
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    let start = Instant::now();

    let TimedOutput::Completed(output) =
        output_with_timeout(&mut command, Duration::from_secs(5)).unwrap()
    else {
        panic!("shell unexpectedly timed out");
    };
    assert!(output.status.success());
    assert!(start.elapsed() < Duration::from_secs(1));

    thread::sleep(Duration::from_millis(1_100));
    assert!(!survivor_marker.exists(), "descendant survived shell exit");
    assert!(fs::read_dir(temp.path()).unwrap().next().is_none());
}
