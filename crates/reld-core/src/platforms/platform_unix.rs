//! Building blocks shared by every unix host tree. Each unix leaf re-exports all of these.

pub mod fs {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    /// Writes all of `buf` at `offset` without using the file cursor.
    pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
    }

    /// Adds execute permission wherever the file currently has read permission.
    pub fn make_executable(file: &File) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let mut permissions = file.metadata()?.permissions();
        let mode = permissions.mode();
        permissions.set_mode(mode | ((mode & 0o444) >> 2));
        file.set_permissions(permissions)
    }

    /// Creates a symlink at `dest_path` pointing to `target`.
    pub fn create_symlink(target: &Path, dest_path: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, dest_path)
    }
}

pub mod host {
    use crate::platforms::host::HostLibc;

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

    /// Whether the host has a usable temporary directory.
    pub const HAS_TEMP_DIR: bool = true;
}

pub mod linker_plugin {
    use std::ffi::c_int;
    use std::fs::File;

    /// Whether this host can load GCC-compatible LTO linker plugins.
    pub const SUPPORTED: bool = true;

    /// C `off_t` in the GCC linker plugin ABI.
    pub type OffT = libc::off_t;

    /// Raw descriptor handed to a plugin.
    #[must_use]
    pub fn file_descriptor(file: &File) -> c_int {
        std::os::fd::AsRawFd::as_raw_fd(file)
    }

    /// Raises the soft open-file limit to the hard limit.
    pub fn increase_file_limit() -> std::io::Result<()> {
        use nix::sys::resource::Resource;

        let (_soft, hard) = nix::sys::resource::getrlimit(Resource::RLIMIT_NOFILE)
            .map_err(std::io::Error::from)?;
        nix::sys::resource::setrlimit(Resource::RLIMIT_NOFILE, hard, hard)
            .map_err(std::io::Error::from)?;
        Ok(())
    }
}

pub mod path {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::path::PathBuf;

    /// Converts raw path bytes (e.g. an archive member name) to a path.
    #[must_use]
    pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        Path::new(OsStr::from_bytes(bytes)).to_path_buf()
    }
}

pub mod process {
    use crate::platforms::process::LinkerFork;
    use libc::c_char;
    use libc::pid_t;
    use std::ffi::c_int;
    use std::ffi::c_void;
    use std::io;
    use std::process::Child;
    use std::process::Command;
    use std::process::ExitStatus;

    /// Lets a forked linker child tell its parent that the link succeeded.
    pub struct ParentNotifier {
        fds: [c_int; 2],
    }

    impl ParentNotifier {
        /// Informs the parent process that the work of the linker is done and that it succeeded.
        pub fn notify_done(&self) {
            let fds = &self.fds;
            unsafe {
                libc::close(fds[0]);
                let stream = libc::fdopen(fds[1], "w".as_ptr().cast::<c_char>());
                let bytes: [u8; 1] = *b"X";
                libc::fwrite(bytes.as_ptr().cast::<c_void>(), 1, 1, stream);
                libc::fclose(stream);
                libc::close(libc::STDOUT_FILENO);
                libc::close(libc::STDERR_FILENO);
            }
        }
    }

    /// Forks so the link can run in a child while the parent exits as soon as outputs are written.
    ///
    /// # Safety
    /// Must be called before any threads have been spawned.
    pub unsafe fn fork_linker() -> io::Result<LinkerFork> {
        let mut fds: [c_int; 2] = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // Safety: the caller guarantees that no threads have been spawned yet.
        match unsafe { libc::fork() } {
            0 => Ok(LinkerFork::Child(ParentNotifier { fds })),
            -1 => Ok(LinkerFork::Unavailable),
            pid => Ok(LinkerFork::Parent(wait_for_child_done(&fds, pid))),
        }
    }

    /// Wait for the child process to signal it is done, by sending a byte on the pipe. In the case
    /// the child crashes, or exits via some path that doesn't send a byte, then the pipe will be
    /// closed and we'll then wait for the subprocess to exit, returning its exit code.
    fn wait_for_child_done(fds: &[c_int], child_pid: pid_t) -> i32 {
        unsafe {
            libc::close(fds[1]);
            let stream = libc::fdopen(fds[0], "r".as_ptr().cast::<c_char>());

            let mut response: [u8; 1] = [0u8; 1];
            if libc::fread(response.as_mut_ptr().cast::<c_void>(), 1, 1, stream) == 1 {
                0
            } else {
                let mut status: libc::c_int = -1i32;
                libc::waitpid(child_pid, &raw mut status, 0);
                libc::WEXITSTATUS(status)
            }
        }
    }

    /// The signal that terminated the process, if any.
    #[must_use]
    pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
        std::os::unix::process::ExitStatusExt::signal(status)
    }

    /// Makes the spawned command lead a private process group.
    pub fn isolate_process_group(command: &mut Command) {
        std::os::unix::process::CommandExt::process_group(command, 0);
    }

    /// Kills `child` and everything in its private process group.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub fn kill_process_tree(child: &mut Child) -> io::Result<()> {
        kill_process_group(child.id())
    }

    /// After `child` exited normally, kills anything it left in its private process group.
    pub fn reap_process_group(child: &Child) -> io::Result<()> {
        kill_process_group(child.id())
    }

    /// Whether `kill_process_tree` also terminates grandchildren.
    pub const KILLS_PROCESS_TREES: bool = true;

    fn kill_process_group(id: u32) -> io::Result<()> {
        let process_group = -id.cast_signed();
        // SAFETY: `kill` does not dereference pointers. The negative PID deliberately addresses the
        // private process group established by `isolate_process_group`.
        if unsafe { libc::kill(process_group, libc::SIGKILL) } == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
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
