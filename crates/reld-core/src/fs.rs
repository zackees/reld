//! Filesystem abstraction used by the linker.
//!
//! The main output is exposed as a sized random-access byte buffer because linker writers fill
//! disjoint regions in parallel. Auxiliary outputs are written as complete byte slices.

use crate::error::Context as _;
use crate::error::Result;
#[cfg(target_os = "linux")]
use crate::timing_phase;
use memmap2::Mmap;
use memmap2::MmapOptions;
use std::fs::File;
use std::io::ErrorKind;
use std::io::Write as _;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(target_os = "linux")]
const LARGE_EXT4_BUFFERED_OUTPUT_MIN: u64 = 256 * 1024 * 1024;
#[cfg(target_os = "linux")]
const LARGE_EXT4_BUFFERED_OUTPUT_MAX: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReplacementMode {
    /// The existing output file, if any, will be unlinked (deleted) and a new file with the same
    /// name put in its place. Any hard links to the file will not be affected.
    UnlinkAndReplace,

    /// The existing output file, if any, will be edited in-place. Any hard links to the file will
    /// update accordingly. If the file is locked due to currently being executed, then our write
    /// will fail.
    UpdateInPlace,

    /// As for `UpdateInPlace`, but if we get an error opening the file for write, fallback to
    /// unlinking and replacing.
    UpdateInPlaceWithFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileWriteMode {
    Mmap,
    BufferThenWrite,
}

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions {
    pub size: u64,
    pub file_replacement_mode: FileReplacementMode,
    pub write_mode: Option<FileWriteMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Other,
}

/// An opened linker input. Implementations own the storage returned by [`InputFile::bytes`].
pub trait InputFileData: Send + Sync + std::fmt::Debug {
    fn bytes(&self) -> &[u8];

    /// Returns whether the input still has the same identity as when it was opened.
    fn verify_unchanged(&self) -> std::io::Result<bool> {
        Ok(true)
    }
}

/// A sized, random-access linker output.
pub trait OutputFileData: Send {
    /// Returns the complete output buffer for reading.
    fn bytes(&self) -> &[u8];

    /// Returns the output buffer for random-access writing.
    fn bytes_mut(&mut self) -> &mut [u8];

    /// Persist the bytes and apply final file attributes.
    fn finish(self) -> Result;

    /// Invalidate any OS caches that may have observed partially written output.
    fn invalidate(&mut self, _len: usize) {}
}

/// Filesystem services needed by the core linker.
///
/// # Examples
///
/// ```
/// use reld_core::{FileSystem, FileType, InputFileData, Linker, OutputFileData, OutputOptions};
/// use object::write::{Object, StandardSection, Symbol, SymbolSection};
/// use object::{Architecture, BinaryFormat, Endianness, SymbolFlags, SymbolKind, SymbolScope};
/// use std::collections::HashMap;
/// use std::fs::File;
/// use std::mem;
/// use std::path::{Path, PathBuf};
/// use std::sync::{Arc, Mutex};
///
/// // A small in-memory filesystem backed by a dictionary of path -> contents.
/// #[derive(Clone, Default)]
/// pub(crate) struct InMemoryFileSystem {
///     pub(crate) files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
/// }
///
/// #[derive(Debug)]
/// struct Input(Vec<u8>);
///
/// impl InputFileData for Input {
///     fn bytes(&self) -> &[u8] {
///         &self.0
///     }
/// }
///
/// struct Output {
///     path: PathBuf,
///     bytes: Vec<u8>,
///     files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
/// }
///
/// impl OutputFileData for Output {
///     fn bytes(&self) -> &[u8] {
///         &self.bytes
///     }
///
///     fn bytes_mut(&mut self) -> &mut [u8] {
///         &mut self.bytes
///     }
///
///     fn finish(mut self) -> reld_core::error::Result {
///         let data = mem::take(&mut self.bytes);
///         self.files.lock().unwrap().insert(self.path.clone(), data);
///         Ok(())
///     }
/// }
///
/// impl FileSystem for InMemoryFileSystem {
///     type Input = Input;
///     type Output = Output;
///
///     fn open_input(
///         &self,
///         path: &Path,
///         _prepopulate_maps: bool,
///     ) -> reld_core::error::Result<(Self::Input, Option<Arc<File>>)> {
///         let bytes = self
///             .files
///             .lock()
///             .unwrap()
///             .get(&path.to_path_buf())
///             .cloned()
///             .ok_or_else(|| reld_core::error!("No such in-memory file: {}", path.display()))?;
///         Ok((Input(bytes), None))
///     }
///
///     fn file_type(&self, path: &Path) -> std::io::Result<FileType> {
///         if self.files.lock().unwrap().contains_key(&path.to_path_buf()) {
///             Ok(FileType::File)
///         } else {
///             Err(std::io::Error::new(
///                 std::io::ErrorKind::NotFound,
///                 "no such in-memory file",
///             ))
///         }
///     }
///
///     fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
///         Ok(path.to_path_buf())
///     }
///
///     fn rename_file(&self, path: &Path, new_path: &Path) -> std::io::Result<()> {
///         let mut guard = self.files.lock().unwrap();
///         let Some(data) = guard.remove(&path.to_path_buf()) else {
///             return Err(std::io::Error::new(
///                 std::io::ErrorKind::NotFound,
///                 "no such in-memory file",
///             ));
///         };
///         guard.insert(new_path.to_path_buf(), data);
///
///         Ok(())
///     }
///
///     fn remove_file(&self, path: &Path) -> std::io::Result<()> {
///         self.files
///             .lock()
///             .unwrap()
///             .remove(&path.to_path_buf())
///             .map(|_| ())
///             .ok_or_else(|| {
///                 std::io::Error::new(std::io::ErrorKind::NotFound, "no such in-memory file")
///             })
///     }
///
///     fn create_output(
///         &self,
///         path: Arc<Path>,
///         options: OutputOptions,
///     ) -> reld_core::error::Result<Self::Output> {
///         let size = usize::try_from(options.size)
///             .map_err(|_| reld_core::error!("output is too large for this platform"))?;
///         Ok(Output {
///             path: path.to_path_buf(),
///             bytes: vec![0; size],
///             files: Arc::clone(&self.files),
///         })
///     }
///
///     fn write_auxiliary(&self, path: &Path, bytes: &[u8]) -> reld_core::error::Result {
///         self.files
///             .lock()
///             .unwrap()
///             .insert(path.to_path_buf(), bytes.to_vec());
///         Ok(())
///     }
/// }
///
/// fn create_main_object() -> object::write::Result<Vec<u8>> {
///     let mut object = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
///     let data = object.section_id(StandardSection::Data);
///     let symbol = object.add_symbol(Symbol {
///         name: b"foo".to_vec(),
///         value: 0,
///         size: 0,
///         kind: SymbolKind::Data,
///         scope: SymbolScope::Dynamic,
///         weak: false,
///         section: SymbolSection::Undefined,
///         flags: SymbolFlags::None,
///     });
///     object.add_symbol_data(symbol, data, &42_u32.to_le_bytes(), 4);
///     object.write()
/// }
///
/// fn run() -> reld_core::error::Result {
///     let fs = InMemoryFileSystem::default();
///
///     fs.files
///         .lock()
///         .unwrap()
///         .insert(PathBuf::from("main.o"), create_main_object()?);
///
///     let arguments = [
///         "reld",
///         "-m",
///         "elf_x86_64",
///         "-shared",
///         "main.o",
///         "-o",
///         "libx.so",
///     ];
///     let get_arguments = || arguments.into_iter();
///     let mut args = reld_core::Args::new(get_arguments)?;
///     args.parse(get_arguments)?;
///
///     let linker = Linker::with_file_system(fs.clone());
///     linker.run(&args)?;
///
///     let output = fs
///         .files
///         .lock()
///         .unwrap()
///         .get(Path::new("libx.so"))
///         .cloned()
///         .ok_or_else(|| reld_core::error!("linker did not create libx.so"))?;
///     // std::fs::write("libx.so", &output)?;
///     Ok(())
/// }
/// ```
pub trait FileSystem: Send + Sync + 'static {
    type Input: InputFileData;
    type Output: OutputFileData;

    /// Opens an input and optionally requests that its pages be populated in advance.
    fn open_input(
        &self,
        path: &Path,
        prepopulate_maps: bool,
    ) -> Result<(Self::Input, Option<Arc<File>>)>;

    /// Returns the type of the file at `path`.
    fn file_type(&self, path: &Path) -> std::io::Result<FileType>;

    /// Resolves symbolic links and returns the canonical absolute path.
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf>;

    /// Removes a file.
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;

    /// Rename an existing file to a new path.
    fn rename_file(&self, path: &Path, new_path: &Path) -> std::io::Result<()>;

    /// Creates the sized random-access output.
    fn create_output(&self, path: Arc<Path>, options: OutputOptions) -> Result<Self::Output>;

    /// Writes a complete auxiliary output.
    fn write_auxiliary(&self, path: &Path, bytes: &[u8]) -> Result;
}

/// The normal host operating-system filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsFileSystem;

impl OsFileSystem {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[derive(Debug)]
struct OsInputBytes(Mmap);

#[derive(Debug)]
pub struct OsInputFile {
    bytes: OsInputBytes,
    path: PathBuf,
    /// The modification timestamp of the input file just before we opened it. We expect our input
    /// files not to change while we're running.
    modification_time: std::time::SystemTime,
}

impl Deref for OsInputBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl InputFileData for OsInputFile {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn verify_unchanged(&self) -> std::io::Result<bool> {
        Ok(std::fs::metadata(&self.path)?.modified()? == self.modification_time)
    }
}

enum OsOutputBuffer {
    Mmap(memmap2::MmapMut),
    InMemory(Vec<u8>),
    #[cfg(target_os = "linux")]
    Splice(memmap2::MmapMut),
}

pub struct OsOutputFile {
    file: File,
    buffer: OsOutputBuffer,
    path: Arc<Path>,
}

impl OutputFileData for OsOutputFile {
    fn bytes(&self) -> &[u8] {
        match &self.buffer {
            OsOutputBuffer::Mmap(mmap) => mmap,
            OsOutputBuffer::InMemory(bytes) => bytes,
            #[cfg(target_os = "linux")]
            OsOutputBuffer::Splice(bytes) => bytes,
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        match &mut self.buffer {
            OsOutputBuffer::Mmap(mmap) => mmap,
            OsOutputBuffer::InMemory(bytes) => bytes,
            #[cfg(target_os = "linux")]
            OsOutputBuffer::Splice(bytes) => bytes,
        }
    }

    fn finish(self) -> Result {
        if let OsOutputBuffer::InMemory(bytes) = &self.buffer {
            (&self.file)
                .write_all(bytes)
                .with_context(|| format!("Failed to write to {}", self.path.display()))?;
        }
        #[cfg(target_os = "linux")]
        if let OsOutputBuffer::Splice(bytes) = &self.buffer {
            timing_phase!("Splice buffered output", bytes = bytes.len());
            match splice_buffer_to_file(&self.file, bytes) {
                Ok(SpliceOutcome::Complete) => {}
                Ok(SpliceOutcome::Fallback) => {
                    use std::os::unix::fs::FileExt as _;

                    self.file.write_all_at(bytes, 0).with_context(|| {
                        format!(
                            "Failed to write to {} after splice fallback",
                            self.path.display()
                        )
                    })?;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Failed to splice to {}", self.path.display()));
                }
            }
        }

        // Making the file executable is best-effort only. For example if we're writing to a pipe or
        // something, it isn't going to work and that's OK.
        let _ = make_executable(&self.file);

        Ok(())
    }

    fn invalidate(&mut self, len: usize) {
        #[cfg(target_os = "macos")]
        if let OsOutputBuffer::Mmap(output) = &mut self.buffer {
            unsafe {
                libc::msync(output.as_mut_ptr().cast(), len, libc::MS_INVALIDATE);
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = len;
    }
}

impl FileSystem for OsFileSystem {
    type Input = OsInputFile;
    type Output = OsOutputFile;

    fn open_input(
        &self,
        path: &Path,
        #[allow(unused_variables)] prepopulate_maps: bool,
    ) -> Result<(Self::Input, Option<Arc<File>>)> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open input file `{}`", path.display()))?;

        let modification_time = file
            .metadata()
            .and_then(|meta| meta.modified())
            .with_context(|| {
                format!("Failed to read file modification time `{}`", path.display())
            })?;

        let bytes = {
            // Safety: Unfortunately, this is a bit of a compromise. Basically this is only safe if
            // our users manage to avoid editing the input files while we've got them
            // mapped. It'd be great if there were a way to protect against unsoundness
            // when the input files were modified externally, but there isn't - at least
            // on Linux. Not only could the bytes change without notice, but the mapped
            // file could be truncated causing any access to result in a SIGBUS.
            //
            // For our use case, mmap just has too many advantages. There are likely large parts of
            // our input files that we don't need to read, so reading all our input
            // files up front isn't really an option. Reading just the parts we need
            // might be an option, but would add substantial complexity. Also, using
            // mmap means that if the system needs to reclaim memory, it can just
            // release some of our pages.

            let mut mmap_options = memmap2::MmapOptions::new();

            // Prepopulating maps generally slows things down, so is off by default, however it's
            // useful when profiling, since it means that you don't see false positive
            // slowness in the parts of the code that first read a bit of memory.
            if prepopulate_maps {
                mmap_options.populate();
            }

            let bytes = unsafe { mmap_options.map(&file) }
                .with_context(|| format!("Failed to mmap input file `{}`", path.display()))?;

            OsInputBytes(bytes)
        };

        Ok((
            OsInputFile {
                bytes,
                path: path.to_owned(),
                modification_time,
            },
            Some(Arc::new(file)),
        ))
    }

    fn file_type(&self, path: &Path) -> std::io::Result<FileType> {
        let ty = std::fs::metadata(path)?.file_type();
        Ok(if ty.is_file() {
            FileType::File
        } else if ty.is_dir() {
            FileType::Directory
        } else {
            FileType::Other
        })
    }

    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn rename_file(&self, path: &Path, new_path: &Path) -> std::io::Result<()> {
        std::fs::rename(path, new_path)
    }

    fn create_output(&self, path: Arc<Path>, options: OutputOptions) -> Result<Self::Output> {
        let mut open_options = std::fs::OpenOptions::new();

        match options.file_replacement_mode {
            FileReplacementMode::UnlinkAndReplace => {
                open_options.truncate(true);
            }
            FileReplacementMode::UpdateInPlace | FileReplacementMode::UpdateInPlaceWithFallback => {
                open_options.truncate(false);
            }
        }

        let file = match open_options.read(true).write(true).create(true).open(&path) {
            Ok(file) => file,
            Err(error) => {
                // Retry open operation with UnlinkAndReplace if it's an ETXTBSY error and
                // falllback is permitted.
                if error.kind() == ErrorKind::ExecutableFileBusy
                    && matches!(
                        options.file_replacement_mode,
                        FileReplacementMode::UpdateInPlaceWithFallback
                    )
                {
                    // If the file is being executed, we can't modify it, but we can delete it.
                    std::fs::remove_file(&path)?;
                    open_options.create(true).open(&path)?
                } else {
                    return Err(error)
                        .with_context(|| format!("Failed to open `{}`", path.display()));
                }
            }
        };

        let file_write_mode = options
            .write_mode
            .unwrap_or_else(|| default_file_write_mode_for_file(&file, options.size));

        let buffer = match file_write_mode {
            FileWriteMode::Mmap => {
                // For some types of output file (e.g. character devices) we can't mmap, so we try
                // to mmap the file and if it fails, fall back to non-mmapped output.
                if file.set_len(options.size).is_ok() {
                    match unsafe { MmapOptions::new().map_mut(&file) } {
                        Ok(mmap) => OsOutputBuffer::Mmap(mmap),
                        Err(_) => OsOutputBuffer::InMemory(vec![0; options.size as usize]),
                    }
                } else {
                    OsOutputBuffer::InMemory(vec![0; options.size as usize])
                }
            }
            FileWriteMode::BufferThenWrite => {
                // Try to set the length of the file. We ignore failures here because it's expected
                // to fail for some types of files, e.g. /dev/null. If there's actually a problem
                // writing to the file, we'll discover that when we go to write the content later
                // on.
                let _ = file.set_len(options.size);
                #[cfg(target_os = "linux")]
                if should_use_splice_buffer_for_file(&file, options.size, file_write_mode) {
                    let size = usize::try_from(options.size)
                        .context("Output is too large for the current address space")?;
                    OsOutputBuffer::Splice(
                        memmap2::MmapMut::map_anon(size)
                            .context("Failed to allocate splice-capable output buffer")?,
                    )
                } else {
                    OsOutputBuffer::InMemory(vec![0; options.size as usize])
                }
                #[cfg(not(target_os = "linux"))]
                {
                    OsOutputBuffer::InMemory(vec![0; options.size as usize])
                }
            }
        };
        Ok(OsOutputFile { file, buffer, path })
    }

    fn write_auxiliary(&self, path: &Path, bytes: &[u8]) -> Result {
        let file = File::create(path)?;
        (&file).write_all(bytes)?;
        Ok(())
    }
}

fn default_file_write_mode_for_file(file: &std::fs::File, output_size: u64) -> FileWriteMode {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    {
        default_file_write_mode_for_filesystem_type(
            nix::sys::statfs::fstatfs(file)
                .map(|stat| stat.filesystem_type())
                .ok(),
            output_size,
        )
    }
    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    {
        let _ = (file, output_size);
        FileWriteMode::Mmap
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn default_file_write_mode_for_filesystem_type(
    filesystem_type: Option<nix::sys::statfs::FsType>,
    output_size: u64,
) -> FileWriteMode {
    // Preserve the existing Btrfs/vfat policy at every size.
    if let Some(nix::sys::statfs::BTRFS_SUPER_MAGIC | nix::sys::statfs::MSDOS_SUPER_MAGIC) =
        filesystem_type
    {
        return FileWriteMode::BufferThenWrite;
    }

    #[cfg(target_os = "linux")]
    if filesystem_type == Some(nix::sys::statfs::EXT4_SUPER_MAGIC)
        && (LARGE_EXT4_BUFFERED_OUTPUT_MIN..=LARGE_EXT4_BUFFERED_OUTPUT_MAX).contains(&output_size)
    {
        // Exact captured-link trials consistently show that writing one completed buffer is
        // faster than faulting and dirtying a large ext4 mmap during parallel section writes.
        // Keep the policy inside the measured range to avoid unbounded memory use on huge links.
        return FileWriteMode::BufferThenWrite;
    }

    let _ = output_size;
    FileWriteMode::Mmap
}

#[cfg(target_os = "linux")]
fn should_use_splice_buffer_for_filesystem_type(
    filesystem_type: Option<nix::sys::statfs::FsType>,
    output_size: u64,
    write_mode: FileWriteMode,
) -> bool {
    write_mode == FileWriteMode::BufferThenWrite
        && filesystem_type == Some(nix::sys::statfs::EXT4_SUPER_MAGIC)
        && (LARGE_EXT4_BUFFERED_OUTPUT_MIN..=LARGE_EXT4_BUFFERED_OUTPUT_MAX).contains(&output_size)
}

#[cfg(target_os = "linux")]
fn should_use_splice_buffer_for_file(
    file: &std::fs::File,
    output_size: u64,
    write_mode: FileWriteMode,
) -> bool {
    should_use_splice_buffer_for_filesystem_type(
        nix::sys::statfs::fstatfs(file)
            .map(|stat| stat.filesystem_type())
            .ok(),
        output_size,
        write_mode,
    )
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpliceOutcome {
    Complete,
    Fallback,
}

#[cfg(target_os = "linux")]
fn splice_buffer_to_file(file: &File, bytes: &[u8]) -> std::io::Result<SpliceOutcome> {
    use std::os::fd::AsRawFd as _;
    use std::os::fd::FromRawFd as _;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt as _;

    // SAFETY: sysconf reads process configuration for a constant selector and dereferences no
    // caller-provided pointers.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 || bytes.as_ptr().align_offset(page_size as usize) != 0 {
        return Ok(SpliceOutcome::Fallback);
    }
    let page_size = page_size as usize;
    let aligned_len = bytes.len() / page_size * page_size;
    if aligned_len == 0 {
        return Ok(SpliceOutcome::Fallback);
    }

    let mut raw_pipe = [-1; 2];
    // SAFETY: raw_pipe points to writable storage for exactly the two descriptors pipe2 returns.
    if unsafe { libc::pipe2(raw_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Ok(SpliceOutcome::Fallback);
    }
    // SAFETY: successful pipe2 returned two distinct, owned descriptors, each wrapped exactly
    // once here.
    let pipe_read = unsafe { OwnedFd::from_raw_fd(raw_pipe[0]) };
    // SAFETY: see above; the write descriptor is distinct from the read descriptor.
    let pipe_write = unsafe { OwnedFd::from_raw_fd(raw_pipe[1]) };
    // SAFETY: pipe_write is a live pipe descriptor and F_SETPIPE_SZ takes a scalar size. Raising
    // the capacity is only an optimization, so permission or policy failures are harmless.
    unsafe {
        libc::fcntl(pipe_write.as_raw_fd(), libc::F_SETPIPE_SZ, 1024 * 1024);
    }
    // SAFETY: pipe_write remains a live pipe descriptor and F_GETPIPE_SZ takes no third argument.
    let pipe_capacity = unsafe { libc::fcntl(pipe_write.as_raw_fd(), libc::F_GETPIPE_SZ) };
    if pipe_capacity <= 0 {
        return Ok(SpliceOutcome::Fallback);
    }
    // Gifted requests must contain complete pages. The pipe is empty before each vmsplice call,
    // so limiting the request to its actual page-rounded capacity prevents a blocking producer
    // from waiting for a consumer that cannot run until vmsplice returns.
    let gift_request_limit = pipe_capacity as usize / page_size * page_size;
    if gift_request_limit == 0 {
        return Ok(SpliceOutcome::Fallback);
    }

    // Probe one page without gifting ownership. If either primitive is unavailable, the buffer is
    // still wholly ours and the caller can safely overwrite the output through write_all_at.
    let mut output_offset = 0_i64;
    if transfer_splice_range(
        &pipe_read,
        &pipe_write,
        file,
        &bytes[..page_size],
        &mut output_offset,
        false,
        page_size,
        page_size,
    )
    .is_err()
    {
        return Ok(SpliceOutcome::Fallback);
    }

    // SPLICE_F_GIFT promises that these complete anonymous pages will never be modified again.
    // finish consumes the output, we wait for every gifted byte to leave the pipe, and the mapping
    // is only unmapped after this function returns. Any error after this point is therefore fatal:
    // falling back would access pages whose ownership has already been gifted.
    transfer_splice_range(
        &pipe_read,
        &pipe_write,
        file,
        &bytes[page_size..aligned_len],
        &mut output_offset,
        true,
        gift_request_limit,
        page_size,
    )?;

    if aligned_len < bytes.len() {
        file.write_all_at(&bytes[aligned_len..], aligned_len as u64)?;
    }
    Ok(SpliceOutcome::Complete)
}

#[cfg(target_os = "linux")]
fn transfer_splice_range(
    pipe_read: &std::os::fd::OwnedFd,
    pipe_write: &std::os::fd::OwnedFd,
    output: &File,
    bytes: &[u8],
    output_offset: &mut i64,
    gift: bool,
    supply_limit: usize,
    page_size: usize,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;

    complete_splice_range(
        bytes.len(),
        |offset, remaining| {
            let request_len = remaining.min(supply_limit);
            let iov = libc::iovec {
                iov_base: bytes[offset..].as_ptr().cast_mut().cast(),
                iov_len: request_len,
            };
            let flags = if gift { libc::SPLICE_F_GIFT } else { 0 };
            // SAFETY: iov references the live output mapping for this blocking call. Its length
            // is bounded by the empty pipe's capacity; gifted requests start on page boundaries,
            // contain whole pages, and the caller prevents every subsequent mutation.
            let result =
                unsafe { libc::vmsplice(pipe_write.as_raw_fd(), &raw const iov, 1, flags) };
            if result < 0 {
                Err(std::io::Error::last_os_error())
            } else if gift && !(result as usize).is_multiple_of(page_size) {
                Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    "vmsplice accepted a non-page-aligned gifted range",
                ))
            } else {
                Ok(result as usize)
            }
        },
        |remaining| {
            // SAFETY: both descriptors remain owned for this call and output_offset points to
            // live, writable storage whose value the kernel advances after a successful transfer.
            let result = unsafe {
                libc::splice(
                    pipe_read.as_raw_fd(),
                    std::ptr::null_mut(),
                    output.as_raw_fd(),
                    output_offset,
                    remaining,
                    libc::SPLICE_F_MOVE,
                )
            };
            if result < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(result as usize)
            }
        },
    )
}

#[cfg(target_os = "linux")]
fn complete_splice_range(
    len: usize,
    mut supply: impl FnMut(usize, usize) -> std::io::Result<usize>,
    mut drain: impl FnMut(usize) -> std::io::Result<usize>,
) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < len {
        let supplied = retry_interrupted(|| supply(offset, len - offset))?;
        require_progress("vmsplice", supplied)?;

        let mut consumed = 0;
        while consumed < supplied {
            let moved = retry_interrupted(|| drain(supplied - consumed))?;
            require_progress("splice", moved)?;
            consumed += moved;
        }
        offset += supplied;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn retry_interrupted(
    mut operation: impl FnMut() -> std::io::Result<usize>,
) -> std::io::Result<usize> {
    loop {
        match operation() {
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

#[cfg(target_os = "linux")]
fn require_progress(operation: &'static str, transferred: usize) -> std::io::Result<()> {
    if transferred == 0 {
        Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            format!("{operation} transferred no bytes"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn large_linux_ext4_outputs_use_the_measured_buffered_policy() {
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
                LARGE_EXT4_BUFFERED_OUTPUT_MIN,
            ),
            FileWriteMode::BufferThenWrite
        );
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
                LARGE_EXT4_BUFFERED_OUTPUT_MAX,
            ),
            FileWriteMode::BufferThenWrite
        );
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
                LARGE_EXT4_BUFFERED_OUTPUT_MAX + 1,
            ),
            FileWriteMode::Mmap
        );
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
                64 * 1024 * 1024,
            ),
            FileWriteMode::Mmap
        );
        assert_eq!(
            default_file_write_mode_for_filesystem_type(None, 256 * 1024 * 1024),
            FileWriteMode::Mmap
        );
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::BTRFS_SUPER_MAGIC),
                64 * 1024 * 1024,
            ),
            FileWriteMode::BufferThenWrite
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn benchmark_sized_ext4_buffer_uses_page_splicing() {
        assert!(should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MIN,
            FileWriteMode::BufferThenWrite,
        ));
        assert!(should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MAX,
            FileWriteMode::BufferThenWrite,
        ));
        assert!(!should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MIN - 1,
            FileWriteMode::BufferThenWrite,
        ));
        assert!(!should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MAX + 1,
            FileWriteMode::BufferThenWrite,
        ));
        assert!(!should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MIN,
            FileWriteMode::Mmap,
        ));
        assert!(!should_use_splice_buffer_for_filesystem_type(
            Some(nix::sys::statfs::BTRFS_SUPER_MAGIC),
            LARGE_EXT4_BUFFERED_OUTPUT_MIN,
            FileWriteMode::BufferThenWrite,
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn splice_buffer_exceeding_pipe_capacity_preserves_pages_and_unaligned_tail() {
        use std::io::Read as _;
        use std::io::Seek as _;
        use std::io::SeekFrom;

        // This exceeds even the requested 1 MiB pipe capacity, proving the implementation
        // interleaves bounded supplies and drains instead of blocking on one giant vmsplice.
        let len = 2 * 1024 * 1024 + 137;
        let mut bytes = memmap2::MmapMut::map_anon(len).unwrap();
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let expected = bytes.to_vec();
        let mut output = tempfile::tempfile().unwrap();
        output.set_len(len as u64).unwrap();

        assert_eq!(
            splice_buffer_to_file(&output, &bytes).unwrap(),
            SpliceOutcome::Complete
        );
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        output.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn splice_range_retries_interrupts_and_short_transfers() {
        let mut supply_calls = 0;
        let mut drain_calls = 0;
        complete_splice_range(
            17,
            |_, remaining| {
                supply_calls += 1;
                if supply_calls == 1 {
                    return Err(std::io::Error::from(ErrorKind::Interrupted));
                }
                Ok(remaining.min(5))
            },
            |remaining| {
                drain_calls += 1;
                if drain_calls == 1 {
                    return Err(std::io::Error::from(ErrorKind::Interrupted));
                }
                Ok(remaining.min(3))
            },
        )
        .unwrap();

        assert!(supply_calls > 4);
        assert!(drain_calls > 7);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn splice_range_rejects_zero_progress_and_partial_failure() {
        let zero = complete_splice_range(8, |_, _| Ok(0), |_| unreachable!()).unwrap_err();
        assert_eq!(zero.kind(), std::io::ErrorKind::WriteZero);

        let mut supplied = false;
        let partial = complete_splice_range(
            8,
            |_, _| {
                supplied = true;
                Ok(4)
            },
            |_| Err(std::io::Error::from(ErrorKind::BrokenPipe)),
        )
        .unwrap_err();
        assert!(supplied);
        assert_eq!(partial.kind(), ErrorKind::BrokenPipe);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsupported_probe_falls_back_before_gifting_pages() {
        // SAFETY: sysconf reads process configuration for a constant selector.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let bytes = memmap2::MmapMut::map_anon(page_size * 2).unwrap();
        let output = File::options().write(true).open("/dev/full").unwrap();

        assert_eq!(
            splice_buffer_to_file(&output, &bytes).unwrap(),
            SpliceOutcome::Fallback
        );
    }
}

/// Make the the supplied file executable by adding execute permissions for all users that have read
/// permissions. On non-Unix platforms, this is a no-op.
pub fn make_executable(_file: &File) -> Result {
    #[cfg(unix)]
    {
        use std::os::unix::prelude::PermissionsExt;
        let mut permissions = _file.metadata()?.permissions();
        let mut mode = PermissionsExt::mode(&permissions);
        // Set execute permission wherever we currently have read permission.
        mode = mode | ((mode & 0o444) >> 2);
        PermissionsExt::set_mode(&mut permissions, mode);
        _file.set_permissions(permissions)?;
    }
    Ok(())
}

pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        std::path::Path::new(OsStr::from_bytes(bytes)).to_path_buf()
    }

    #[cfg(target_os = "wasi")]
    {
        use std::ffi::OsStr;
        use std::os::wasi::ffi::OsStrExt as _;
        std::path::Path::new(OsStr::from_bytes(bytes)).to_path_buf()
    }

    #[cfg(not(any(unix, target_os = "wasi")))]
    {
        let path = std::str::from_utf8(bytes).expect("Invalid UTF-8 in archive path name");
        PathBuf::from(path)
    }
}
