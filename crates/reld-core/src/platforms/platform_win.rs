//! Windows host tree.

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

    /// Writes all of `buf` at `offset`. Each chunk is an overlapped write at an explicit offset, so
    /// concurrent calls on disjoint ranges are safe even though the file cursor moves.
    pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt as _;

        let mut buf = buf;
        let mut offset = offset;
        while !buf.is_empty() {
            match file.seek_write(buf, offset) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                Ok(written) => {
                    buf = &buf[written..];
                    offset += written as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Executable permissions do not exist on this host; does nothing.
    #[allow(clippy::unnecessary_wraps)]
    pub fn make_executable(_file: &File) -> io::Result<()> {
        Ok(())
    }

    /// Creates a directory or file symlink at `dest_path` pointing to `target`.
    pub fn create_symlink(target: &Path, dest_path: &Path) -> io::Result<()> {
        use std::os::windows::fs::FileTypeExt as _;

        let is_dir = std::fs::metadata(target).is_ok_and(|meta| meta.is_dir());
        let is_symlink_dir =
            std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink_dir());
        if is_dir || is_symlink_dir {
            std::os::windows::fs::symlink_dir(target, dest_path)
        } else {
            std::os::windows::fs::symlink_file(target, dest_path)
        }
    }
}

pub mod host {
    use crate::platforms::host::HostLibc;
    use crate::platforms::host::HostOs;

    /// The host operating system.
    #[must_use]
    pub fn os() -> HostOs {
        HostOs::Windows
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
    pub const HAS_TEMP_DIR: bool = true;
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
    use std::path::PathBuf;

    /// Converts path bytes (e.g. an archive member name), which must be UTF-8, to a path.
    #[must_use]
    pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        let path = std::str::from_utf8(bytes).expect("Invalid UTF-8 in archive path name");
        PathBuf::from(path)
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
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    use windows_sys::Win32::System::Console::GetConsoleMode;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
    use windows_sys::Win32::System::Console::SetConsoleMode;

    /// Reports whether stderr renders ANSI, switching virtual-terminal processing on first if
    /// this is a console that supports it.
    ///
    /// Three cases matter here. A Windows console needs `ENABLE_VIRTUAL_TERMINAL_PROCESSING`
    /// before it renders escape sequences, and a console too old to accept that flag must be
    /// reported as plain so reld never writes raw escapes to it. An MSYS2/Cygwin/mintty terminal
    /// is a named pipe rather than a console, so `GetConsoleMode` fails on it even though it
    /// renders ANSI itself. Everything else is a file or a pipe.
    #[must_use]
    pub fn prepare_stderr_for_ansi() -> StderrTerminal {
        if !std::io::stderr().is_terminal() {
            return StderrTerminal::NotATerminal;
        }

        // SAFETY: each call takes a handle owned by the process and, for `GetConsoleMode`, a
        // pointer to a live local that outlives the call.
        unsafe {
            let handle = GetStdHandle(STD_ERROR_HANDLE);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return StderrTerminal::NotATerminal;
            }

            let mut mode = 0;
            if GetConsoleMode(handle, &raw mut mode) == 0 {
                // A terminal that is not a console: an MSYS2/Cygwin/mintty pty.
                return StderrTerminal::Ansi;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
                return StderrTerminal::Ansi;
            }
            if SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
                return StderrTerminal::NotATerminal;
            }
            StderrTerminal::Ansi
        }
    }
}
