//! Linux and Android host tree.

pub mod fs {
    pub use crate::platforms::platform_unix::fs::create_symlink;
    pub use crate::platforms::platform_unix::fs::make_executable;
    pub use crate::platforms::platform_unix::fs::write_all_at;

    use crate::platforms::fs::FilesystemKind;
    use std::fs::File;
    use std::io;

    // nix does not expose this Linux filesystem magic on musl targets.
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    const XFS_SUPER_MAGIC: nix::sys::statfs::FsType = nix::sys::statfs::FsType(0x5846_5342);
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    const XFS_SUPER_MAGIC: nix::sys::statfs::FsType = nix::sys::statfs::XFS_SUPER_MAGIC;

    /// Filesystem holding `file`, or `None` if it cannot be determined.
    #[must_use]
    pub fn filesystem_kind(file: &File) -> Option<FilesystemKind> {
        let filesystem_type = nix::sys::statfs::fstatfs(file).ok()?.filesystem_type();
        // Android only distinguishes Btrfs and vfat, matching its historical output policy.
        Some(match filesystem_type {
            #[cfg(target_os = "linux")]
            nix::sys::statfs::EXT4_SUPER_MAGIC => FilesystemKind::Ext4,
            #[cfg(target_os = "linux")]
            XFS_SUPER_MAGIC => FilesystemKind::Xfs,
            nix::sys::statfs::BTRFS_SUPER_MAGIC => FilesystemKind::Btrfs,
            nix::sys::statfs::MSDOS_SUPER_MAGIC => FilesystemKind::Vfat,
            _ => FilesystemKind::Other,
        })
    }

    /// Preallocates `size` bytes for `file`.
    #[cfg(target_os = "linux")]
    pub fn preallocate(file: &File, size: u64) -> io::Result<()> {
        if size > 0 {
            let len = i64::try_from(size).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Output file is too large for fallocate",
                )
            })?;
            nix::fcntl::fallocate(file, nix::fcntl::FallocateFlags::empty(), 0, len)
                .map_err(io::Error::from)?;
        }
        Ok(())
    }

    /// Preallocates `size` bytes for `file`.
    #[cfg(not(target_os = "linux"))]
    pub fn preallocate(_file: &File, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fallocate is only supported on Linux",
        ))
    }

    /// Asks the kernel to back `mmap` with transparent huge pages.
    #[cfg(target_os = "linux")]
    pub fn advise_huge_pages(mmap: &memmap2::MmapMut) -> io::Result<()> {
        mmap.advise(memmap2::Advice::HugePage)
    }

    /// Asks the kernel to back `mmap` with transparent huge pages.
    #[cfg(not(target_os = "linux"))]
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
        if cfg!(target_os = "android") {
            HostOs::Android
        } else {
            HostOs::Linux
        }
    }

    /// Short flags the host's Clang driver inserts before invoking a non-GNU ld.
    pub const DRIVER_INSERTED_NOOP_SHORT_FLAGS: &[&str] = &[];
}

pub mod linker_plugin {
    pub use crate::platforms::platform_unix::linker_plugin::OffT;
    pub use crate::platforms::platform_unix::linker_plugin::SUPPORTED;
    pub use crate::platforms::platform_unix::linker_plugin::file_descriptor;
    pub use crate::platforms::platform_unix::linker_plugin::increase_file_limit;
}

pub mod path {
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

    pub use self::counters::CounterList;

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    mod counters {
        use crate::args::CounterKind;

        /// Hardware/software performance counters for `--time=cycles,...`.
        pub struct CounterList {
            counters: Vec<perf_event::Counter>,
        }

        impl CounterList {
            /// Builds the counters that the kernel lets us open; unavailable ones are skipped.
            #[must_use]
            pub fn from_kinds(kinds: &[CounterKind]) -> Self {
                let counters = kinds
                    .iter()
                    .filter_map(|kind| {
                        perf_event::Builder::new()
                            .inherit(true)
                            .kind(counter_to_perf_event(*kind))
                            .build()
                            .ok()
                    })
                    .collect();

                CounterList { counters }
            }

            /// Resets and enables every counter.
            pub fn start(&mut self) {
                for counter in &mut self.counters {
                    let _ = counter.reset();
                    let _ = counter.enable();
                }
            }

            /// Disables every counter and returns the values that could be read.
            pub fn disable_and_read(&mut self) -> Vec<u64> {
                self.counters
                    .iter_mut()
                    .filter_map(|counter| {
                        counter.disable().ok().and_then(|()| counter.read().ok())
                    })
                    .collect()
            }
        }

        fn counter_to_perf_event(kind: CounterKind) -> perf_event::events::Event {
            match kind {
                CounterKind::Cycles => perf_event::events::Hardware::CPU_CYCLES.into(),
                CounterKind::Instructions => perf_event::events::Hardware::INSTRUCTIONS.into(),
                CounterKind::CacheMisses => perf_event::events::Hardware::CACHE_MISSES.into(),
                CounterKind::BranchMisses => perf_event::events::Hardware::BRANCH_MISSES.into(),
                CounterKind::PageFaults => perf_event::events::Software::PAGE_FAULTS.into(),
                CounterKind::PageFaultsMinor => perf_event::events::Software::PAGE_FAULTS_MIN.into(),
                CounterKind::PageFaultsMajor => perf_event::events::Software::PAGE_FAULTS_MAJ.into(),
                CounterKind::L1dRead => perf_event::events::Cache {
                    which: perf_event::events::WhichCache::L1D,
                    operation: perf_event::events::CacheOp::READ,
                    result: perf_event::events::CacheResult::ACCESS,
                }
                .into(),
                CounterKind::L1dMiss => perf_event::events::Cache {
                    which: perf_event::events::WhichCache::L1D,
                    operation: perf_event::events::CacheOp::READ,
                    result: perf_event::events::CacheResult::MISS,
                }
                .into(),
            }
        }
    }

    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    mod counters {
        use crate::args::CounterKind;

        /// Performance counters are unavailable here; every read is empty.
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
}
