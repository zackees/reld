//! Host support for GCC-compatible LTO linker plugins.

pub use super::platform_imp::linker_plugin::{
    OffT, SUPPORTED, file_descriptor, increase_file_limit,
};
