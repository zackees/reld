//! Host terminal capabilities for colored diagnostics.

pub use super::platform_imp::term::prepare_stderr_for_ansi;

/// Whether reld's stderr renders ANSI escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrTerminal {
    /// A file, a pipe, or a terminal that cannot render ANSI.
    NotATerminal,
    /// A terminal that renders ANSI escape sequences.
    Ansi,
}
