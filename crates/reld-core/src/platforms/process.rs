//! Host process services: forking the linker, child exit signals, process-group control and
//! performance counters.

pub use super::platform_imp::process::{
    CounterList, KILLS_PROCESS_TREES, ParentNotifier, exit_signal, fork_linker,
    isolate_process_group, kill_process_tree, reap_process_group,
};

/// The outcome of [`fork_linker`].
pub enum LinkerFork {
    /// Running in the forked child. Call `notify_done` once outputs are written.
    Child(ParentNotifier),
    /// Running in the parent after the child reported; the value is the exit code to use
    /// (0 when the child signalled success over the pipe, else the child's exit status).
    Parent(i32),
    /// Forking is unavailable on this host, or `fork()` failed: link in this process instead.
    Unavailable,
}
