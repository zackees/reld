use crate::Args;
use crate::error::Context as _;
use crate::error::Result;
use crate::platforms::process::LinkerFork;

/// Runs the linker in a subprocess if possible.
///
/// This is done by forking a sub-process which runs the linker and waits for communication back
/// from the sub-process (via a pipe) when the main link task is done (the output file has been
/// written, but some shutdown tasks remain.
///
/// Don't call `setup_tracing` or `setup_thread_pool` if using this function, these will be called
/// for you in the subprocess.
///
/// # Safety
/// Must not be called once threads have been spawned. Calling this function from main is generally
/// the best way to ensure this.
pub unsafe fn run_in_subprocess(args: Args) -> Result {
    if !cfg!(feature = "fork") {
        return crate::run(args);
    }

    match subprocess_result(args)? {
        SubprocessResult::Parent(0) => Ok(()),
        SubprocessResult::Parent(exit_code) | SubprocessResult::Child(exit_code) => {
            std::process::exit(exit_code);
        }
    }
}

enum SubprocessResult {
    Parent(i32),
    Child(i32),
}

fn subprocess_result(mut args: Args) -> Result<SubprocessResult> {
    // Safety: The function we're in is private to this module and is only called from
    // run_in_subprocess, which imposed the requirement that threads have not yet been started on
    // its caller.
    let fork = unsafe { crate::platforms::process::fork_linker() }
        .map_err(|error| {
            crate::error!(
                "Error creating pipe. Errno = {:?}",
                error.raw_os_error().unwrap_or(-1)
            )
        })
        .context("make_pipe")?;

    match fork {
        LinkerFork::Child(notifier) => {
            // Fork success in child - Run linker in this process.

            crate::setup_tracing(&args)?;
            let thread_pool = args.common_mut().build_thread_pool()?;
            thread_pool.pool.install(|| -> Result {
                let linker = crate::Linker::new();
                let _outputs = linker.run(&args)?;
                crate::timing::finalise_perfetto_trace()?;
                notifier.notify_done();
                Ok(())
            })?;
            Ok(SubprocessResult::Child(0))
        }
        LinkerFork::Unavailable => {
            // Forking unavailable or failed - Fallback to running linker in this process

            crate::run(args)?;
            Ok(SubprocessResult::Parent(0))
        }
        LinkerFork::Parent(exit_status) => {
            // Fork success in the parent - the child has signalled us it's done
            Ok(SubprocessResult::Parent(exit_status))
        }
    }
}
