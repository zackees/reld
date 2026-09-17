use reld_core::platforms::process::isolate_process_group;
use reld_core::platforms::process::kill_process_tree;
use reld_core::platforms::process::reap_process_group;
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
    // Give the shell and everything it launches a private process group so a timed-out
    // compiler or linker cannot survive after the shell itself is killed.
    isolate_process_group(command);

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = read_in_background(child.stdout.take().expect("stdout was piped"));
    let stderr = read_in_background(child.stderr.take().expect("stderr was piped"));

    let outcome = match child.wait_timeout(timeout)? {
        Some(status) => {
            reap_process_group(&child)?;
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
