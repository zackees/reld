//! WASI host tree.

pub mod fs {
    use crate::platforms::fs::FilesystemKind;
    use std::fs::File;
    use std::io;
    use std::path::Path;

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

    /// Positioned writes are unsupported on this host.
    pub fn write_all_at(_file: &File, _buf: &[u8], _offset: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "positioned writes are not supported on this host",
        ))
    }

    /// Executable permissions do not exist on this host; does nothing.
    #[allow(clippy::unnecessary_wraps)]
    pub fn make_executable(_file: &File) -> io::Result<()> {
        Ok(())
    }

    /// Symlink creation is unsupported on this host.
    pub fn create_symlink(_target: &Path, _dest_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "creating symlinks on wasi not supported on stable rust",
        ))
    }
}

pub mod host {
    use crate::platforms::host::HostLibc;
    use crate::platforms::host::HostOs;

    /// The host operating system.
    #[must_use]
    pub fn os() -> HostOs {
        HostOs::Wasi
    }

    /// The C library environment reld was built against.
    #[must_use]
    pub fn libc() -> HostLibc {
        if cfg!(target_env = "gnu") {
            HostLibc::Gnu
        } else if cfg!(target_env = "musl") {
            HostLibc::Musl
        } else {
            HostLibc::Other
        }
    }

    /// Short flags the host's Clang driver inserts before invoking a non-GNU ld.
    pub const DRIVER_INSERTED_NOOP_SHORT_FLAGS: &[&str] = &[];

    /// Whether the host has a usable temporary directory.
    pub const HAS_TEMP_DIR: bool = false;
}

pub mod linker_plugin {
    use std::ffi::c_int;
    use std::fs::File;

    /// Linker plugins are not supported on this host.
    pub const SUPPORTED: bool = false;

    /// C `off_t` in the GCC linker plugin ABI.
    pub type OffT = i64;

    /// Never reached because [`SUPPORTED`] is false.
    #[must_use]
    pub fn file_descriptor(_file: &File) -> c_int {
        -1
    }

    /// Open-file limits are not adjusted on this host.
    #[allow(clippy::unnecessary_wraps)]
    pub fn increase_file_limit() -> std::io::Result<()> {
        Ok(())
    }
}

pub mod path {
    /// Environment variable the host's dynamic loader searches for shared libraries, and the
    /// toolchain-root-relative directory rustup prepends to it for the tools it runs.
    pub const TOOLCHAIN_DYLIB_SEARCH: (&str, &str) = ("LD_LIBRARY_PATH", "lib");

    use std::ffi::OsStr;
    use std::os::wasi::ffi::OsStrExt as _;
    use std::path::Path;
    use std::path::PathBuf;

    /// Converts raw path bytes (e.g. an archive member name) to a path.
    #[must_use]
    pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        Path::new(OsStr::from_bytes(bytes)).to_path_buf()
    }
}

pub mod process {
    use crate::args::CounterKind;
    use crate::platforms::process::LinkerFork;
    use std::io;
    use std::process::Child;
    use std::process::Command;
    use std::process::ExitStatus;

    /// Uninhabited: forking is unavailable on this host.
    pub enum ParentNotifier {}

    impl ParentNotifier {
        /// Unreachable because no value exists.
        pub fn notify_done(&self) {
            match *self {}
        }
    }

    /// Forking is unavailable on this host.
    ///
    /// # Safety
    /// Must be called before any threads have been spawned.
    #[allow(clippy::unnecessary_wraps)]
    pub unsafe fn fork_linker() -> io::Result<LinkerFork> {
        Ok(LinkerFork::Unavailable)
    }

    /// Processes are not terminated by signals on this host.
    #[must_use]
    pub fn exit_signal(_status: &ExitStatus) -> Option<i32> {
        None
    }

    /// Process groups are not used on this host; does nothing.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub fn isolate_process_group(_command: &mut Command) {}

    /// Kills `child` only; its descendants are not reached.
    pub fn kill_process_tree(child: &mut Child) -> io::Result<()> {
        child.kill()
    }

    /// Process groups are not used on this host; does nothing.
    #[allow(clippy::unnecessary_wraps)]
    pub fn reap_process_group(_child: &Child) -> io::Result<()> {
        Ok(())
    }

    /// Whether `kill_process_tree` also terminates grandchildren.
    pub const KILLS_PROCESS_TREES: bool = false;

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
    use crate::platforms::term::StderrTerminal;
    use std::io::IsTerminal as _;

    /// Reports whether stderr renders ANSI. Terminals here need no preparation.
    #[must_use]
    pub fn prepare_stderr_for_ansi() -> StderrTerminal {
        if std::io::stderr().is_terminal() {
            StderrTerminal::Ansi
        } else {
            StderrTerminal::NotATerminal
        }
    }
}
