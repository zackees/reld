//! Facts about the host reld runs on.

pub use super::platform_imp::host::{DRIVER_INSERTED_NOOP_SHORT_FLAGS, HAS_TEMP_DIR, libc, os};

/// The host operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    Android,
    MacOs,
    Windows,
    FreeBsd,
    Illumos,
    Wasi,
    OtherUnix,
}

/// The C library environment reld was built against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLibc {
    Gnu,
    Musl,
    Other,
}
