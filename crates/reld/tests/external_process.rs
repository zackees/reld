#![cfg_attr(not(unix), allow(dead_code))]

use std::io;
use std::io::Read;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt as _;

pub(crate) enum TimedOutput {
    Completed(Output),
    TimedOut,
}

pub(crate) fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> io::Result<TimedOutput> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        // Give the shell and everything it launches a private process group so a timed-out
        // compiler or linker cannot survive after the shell itself is killed.
        command.process_group(0);
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = read_in_background(child.stdout.take().expect("stdout was piped"));
    let stderr = read_in_background(child.stderr.take().expect("stderr was piped"));

    let outcome = match child.wait_timeout(timeout)? {
        Some(status) => {
            #[cfg(unix)]
            kill_process_group(child.id())?;
            TimedOutput::Completed(output(status, stdout, stderr)?)
        }
        None => {
            kill_process_tree(&mut child)?;
            child.wait()?;
            join_reader(stdout)?;
            join_reader(stderr)?;
            TimedOutput::TimedOut
        }
    };

    Ok(outcome)
}

fn read_in_background<R>(mut reader: R) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn output(
    status: ExitStatus,
    stdout: thread::JoinHandle<io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> io::Result<Output> {
    Ok(Output {
        status,
        stdout: join_reader(stdout)?,
        stderr: join_reader(stderr)?,
    })
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("external-test output reader panicked"))?
}

#[cfg(unix)]
fn kill_process_tree(child: &mut std::process::Child) -> io::Result<()> {
    kill_process_group(child.id())
}

#[cfg(unix)]
fn kill_process_group(id: u32) -> io::Result<()> {
    let process_group = -id.cast_signed();
    // SAFETY: `kill` does not dereference pointers. The negative PID deliberately addresses the
    // private process group established above, not an unrelated process.
    if unsafe { libc::kill(process_group, libc::SIGKILL) } == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn kill_process_tree(child: &mut std::process::Child) -> io::Result<()> {
    child.kill()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
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
}
