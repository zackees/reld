//! Host path conventions.

pub use super::platform_imp::path::path_from_bytes;

/// Suffix of executables on the host (`.exe` on Windows, empty elsewhere).
pub const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;
