//! illumos host tree.

pub mod fs {
    pub use crate::platforms::platform_unix::fs::create_symlink;
    pub use crate::platforms::platform_unix::fs::make_executable;
    pub use crate::platforms::platform_unix::fs::write_all_at;

    use crate::platforms::fs::FilesystemKind;
    use std::fs::File;
    use std::io;

    /// Filesystem holding `file`; not detected on this host.
    #[must_use]
    pub fn filesystem_kind(_file: &File) -> Option<FilesystemKind> {
        None
    }

    /// Preallocation is unsupported on this host.
    pub fn preallocate(_file: &File, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fallocate is only supported on Linux",
        ))
    }

    /// Huge-page advice is unsupported on this host.
    pub fn advise_huge_pages(_mmap: &memmap2::MmapMut) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MADV_HUGEPAGE is only supported on Linux",
        ))
    }

    /// Invalidates OS caches that may have observed partially written mapped output.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub fn invalidate_mapped_output(_mmap: &mut memmap2::MmapMut, _len: usize) {}
}

pub mod host {
    pub use crate::platforms::platform_unix::host::HAS_TEMP_DIR;
    pub use crate::platforms::platform_unix::host::libc;

    use crate::platforms::host::HostOs;

    /// The host operating system.
    #[must_use]
    pub fn os() -> HostOs {
        HostOs::Illumos
    }

    /// Short flags the host's Clang driver inserts before invoking a non-GNU ld. On illumos, the
    /// Clang driver inserts a meaningless `-C` flag.
    pub const DRIVER_INSERTED_NOOP_SHORT_FLAGS: &[&str] = &["C"];
}

pub mod linker_plugin {
    pub use crate::platforms::platform_unix::linker_plugin::OffT;
    pub use crate::platforms::platform_unix::linker_plugin::SUPPORTED;
    pub use crate::platforms::platform_unix::linker_plugin::file_descriptor;
    pub use crate::platforms::platform_unix::linker_plugin::increase_file_limit;
}

pub mod path {
    /// Environment variable the host's dynamic loader searches for shared libraries, and the
    /// toolchain-root-relative directory rustup prepends to it for the tools it runs.
    pub const TOOLCHAIN_DYLIB_SEARCH: (&str, &str) = ("LD_LIBRARY_PATH", "lib");

    pub use crate::platforms::platform_unix::path::path_from_bytes;
}

pub mod process {
    pub use crate::platforms::platform_unix::process::KILLS_PROCESS_TREES;
    pub use crate::platforms::platform_unix::process::ParentNotifier;
    pub use crate::platforms::platform_unix::process::exit_signal;
    pub use crate::platforms::platform_unix::process::fork_linker;
    pub use crate::platforms::platform_unix::process::isolate_process_group;
    pub use crate::platforms::platform_unix::process::kill_process_tree;
    pub use crate::platforms::platform_unix::process::reap_process_group;

    use crate::args::CounterKind;

    /// Performance counters are unavailable on this host; every read is empty.
    pub struct CounterList;

    impl CounterList {
        /// Returns an empty counter list.
        #[must_use]
        pub fn from_kinds(_kinds: &[CounterKind]) -> Self {
            CounterList
        }

        /// Does nothing.
        #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
        pub fn start(&mut self) {}

        /// Returns no counter values.
        #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
        pub fn disable_and_read(&mut self) -> Vec<u64> {
            Vec::new()
        }
    }
}

pub mod term {
    pub use crate::platforms::platform_unix::term::prepare_stderr_for_ansi;
}
