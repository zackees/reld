//! Filesystem abstraction used by the linker.
//!
//! The main output is exposed as a sized random-access byte buffer because linker writers fill
//! disjoint regions in parallel. Auxiliary outputs are written as complete byte slices.

use crate::error::Context as _;
use crate::error::Result;
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
const LARGE_EXT4_OUTPUT_THRESHOLD: u64 = 256 * 1024 * 1024;

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

    /// Reopens the native file backing these bytes when its identity is unchanged.
    fn open_for_direct_copy(&self) -> Option<File> {
        None
    }

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

    /// Returns whether this output can attempt native file-to-file range copies.
    fn supports_file_range_copy(&self) -> bool {
        false
    }

    /// Attempts to copy a byte range directly from an input file into this output.
    ///
    /// Returns `Ok(true)` only when the complete range was copied. Implementations that cannot
    /// provide a direct file-to-file copy return `Ok(false)`. Callers must overwrite the complete
    /// destination range on `Ok(false)`, since an OS copy may have made partial progress.
    fn copy_file_range(
        &mut self,
        _input: &File,
        _input_offset: u64,
        _output_offset: u64,
        _len: usize,
    ) -> std::io::Result<bool> {
        Ok(false)
    }

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
    #[cfg(target_os = "linux")]
    device: u64,
    #[cfg(target_os = "linux")]
    inode: u64,
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

    fn open_for_direct_copy(&self) -> Option<File> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt as _;

            let file = File::open(&self.path).ok()?;
            let metadata = file.metadata().ok()?;
            (metadata.dev() == self.device
                && metadata.ino() == self.inode
                && metadata.len() == self.bytes.len() as u64
                && metadata.modified().ok()? == self.modification_time)
                .then_some(file)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    fn verify_unchanged(&self) -> std::io::Result<bool> {
        Ok(std::fs::metadata(&self.path)?.modified()? == self.modification_time)
    }
}

enum OsOutputBuffer {
    Mmap(memmap2::MmapMut),
    InMemory(Vec<u8>),
}

pub struct OsOutputFile {
    file: File,
    buffer: OsOutputBuffer,
    path: Arc<Path>,
}

#[cfg(target_os = "linux")]
fn complete_file_range_copy(
    mut remaining: usize,
    mut copy: impl FnMut(usize) -> nix::Result<usize>,
) -> bool {
    while remaining > 0 {
        match copy(remaining) {
            Ok(0) => return false,
            Ok(copied) if copied <= remaining => remaining -= copied,
            Ok(_) => return false,
            Err(nix::errno::Errno::EINTR) => {}
            Err(_) => return false,
        }
    }
    true
}

#[cfg(all(test, target_os = "linux"))]
static DIRECT_COPY_SUCCESSES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl OutputFileData for OsOutputFile {
    fn bytes(&self) -> &[u8] {
        match &self.buffer {
            OsOutputBuffer::Mmap(mmap) => mmap,
            OsOutputBuffer::InMemory(bytes) => bytes,
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        match &mut self.buffer {
            OsOutputBuffer::Mmap(mmap) => mmap,
            OsOutputBuffer::InMemory(bytes) => bytes,
        }
    }

    fn supports_file_range_copy(&self) -> bool {
        cfg!(target_os = "linux") && matches!(self.buffer, OsOutputBuffer::Mmap(_))
    }

    fn copy_file_range(
        &mut self,
        input: &File,
        input_offset: u64,
        output_offset: u64,
        len: usize,
    ) -> std::io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            if !self.supports_file_range_copy() {
                return Ok(false);
            }

            let mut input_offset = i64::try_from(input_offset).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "input offset is too large",
                )
            })?;
            let mut output_offset = i64::try_from(output_offset).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "output offset is too large",
                )
            })?;
            let complete = complete_file_range_copy(len, |remaining| {
                nix::fcntl::copy_file_range(
                    input,
                    Some(&mut input_offset),
                    &self.file,
                    Some(&mut output_offset),
                    remaining,
                )
            });
            if complete {
                #[cfg(test)]
                DIRECT_COPY_SUCCESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(complete)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (input, input_offset, output_offset, len);
            Ok(false)
        }
    }

    fn finish(self) -> Result {
        if let OsOutputBuffer::InMemory(bytes) = &self.buffer {
            (&self.file)
                .write_all(bytes)
                .with_context(|| format!("Failed to write to {}", self.path.display()))?;
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
        let file = Arc::new(
            File::open(path)
                .with_context(|| format!("Failed to open input file `{}`", path.display()))?,
        );

        let metadata = file
            .metadata()
            .with_context(|| format!("Failed to read file metadata `{}`", path.display()))?;
        let modification_time = metadata.modified().with_context(|| {
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

            let bytes = unsafe { mmap_options.map(&*file) }
                .with_context(|| format!("Failed to mmap input file `{}`", path.display()))?;

            OsInputBytes(bytes)
        };

        Ok((
            OsInputFile {
                bytes,
                path: path.to_owned(),
                modification_time,
                #[cfg(target_os = "linux")]
                device: {
                    use std::os::unix::fs::MetadataExt as _;
                    metadata.dev()
                },
                #[cfg(target_os = "linux")]
                inode: {
                    use std::os::unix::fs::MetadataExt as _;
                    metadata.ino()
                },
            },
            Some(file),
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

        let should_preallocate = options.write_mode.is_none()
            && should_preallocate_large_ext4_output(&file, options.size);
        let file_write_mode = options
            .write_mode
            .unwrap_or_else(|| default_file_write_mode_for_file(&file, options.size));

        let buffer = match file_write_mode {
            FileWriteMode::Mmap => {
                // For some types of output file (e.g. character devices) we can't mmap, so we try
                // to mmap the file and if it fails, fall back to non-mmapped output.
                if file.set_len(options.size).is_ok() {
                    preallocate_output_file(&file, options.size, should_preallocate);
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
                OsOutputBuffer::InMemory(vec![0; options.size as usize])
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
    _output_size: u64,
) -> FileWriteMode {
    // Preserve the existing Btrfs/vfat policy at every size.
    if let Some(nix::sys::statfs::BTRFS_SUPER_MAGIC | nix::sys::statfs::MSDOS_SUPER_MAGIC) =
        filesystem_type
    {
        FileWriteMode::BufferThenWrite
    } else {
        FileWriteMode::Mmap
    }
}

fn should_preallocate_large_ext4_output(file: &std::fs::File, output_size: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        should_preallocate_large_ext4_output_for_filesystem_type(
            nix::sys::statfs::fstatfs(file)
                .map(|stat| stat.filesystem_type())
                .ok(),
            output_size,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, output_size);
        false
    }
}

#[cfg(target_os = "linux")]
fn should_preallocate_large_ext4_output_for_filesystem_type(
    filesystem_type: Option<nix::sys::statfs::FsType>,
    output_size: u64,
) -> bool {
    filesystem_type == Some(nix::sys::statfs::EXT4_SUPER_MAGIC)
        && output_size >= LARGE_EXT4_OUTPUT_THRESHOLD
}

fn preallocate_output_file(file: &std::fs::File, output_size: u64, should_preallocate: bool) {
    #[cfg(target_os = "linux")]
    if should_preallocate {
        // Reserving extents is substantially cheaper than faulting every output page up front and
        // lets the existing parallel mmap writer avoid block allocation on its critical path.
        let _ = nix::fcntl::fallocate(
            file,
            nix::fcntl::FallocateFlags::empty(),
            0,
            output_size as libc::off_t,
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (file, output_size, should_preallocate);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    static DIRECT_COPY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linker_preserves_a_large_directly_copied_section() {
        use object::Object as _;
        use object::ObjectSection as _;
        use object::ObjectSymbol as _;

        let _direct_copy_guard = DIRECT_COPY_TEST_LOCK.lock().unwrap();
        let direct_copies_before = DIRECT_COPY_SUCCESSES.load(std::sync::atomic::Ordering::Relaxed);
        let directory = tempfile::tempdir().unwrap();
        let input_path = directory.path().join("large.o");
        let output_path = directory.path().join("large.so");
        let payload = (0u8..=255)
            .cycle()
            .take(DIRECT_TEST_SECTION_SIZE)
            .collect::<Vec<_>>();

        let mut input = object::write::Object::new(
            object::BinaryFormat::Elf,
            object::Architecture::X86_64,
            object::Endianness::Little,
        );
        let section = input.add_section(
            Vec::new(),
            b".reld_direct_copy".to_vec(),
            object::SectionKind::ReadOnlyData,
        );
        input.append_section_data(section, &payload, 1);
        input.add_symbol(object::write::Symbol {
            name: b"reld_direct_copy_payload".to_vec(),
            value: 0,
            size: payload.len() as u64,
            kind: object::SymbolKind::Data,
            scope: object::SymbolScope::Dynamic,
            weak: false,
            section: object::write::SymbolSection::Section(section),
            flags: object::SymbolFlags::None,
        });
        std::fs::write(&input_path, input.write().unwrap()).unwrap();

        let arguments = [
            "reld".to_owned(),
            "-shared".to_owned(),
            input_path.to_str().unwrap().to_owned(),
            "-o".to_owned(),
            output_path.to_str().unwrap().to_owned(),
        ];
        let get_arguments = || arguments.iter().map(String::as_str);
        let mut args = crate::Args::new(get_arguments).unwrap();
        args.parse(get_arguments).unwrap();
        let linker = crate::Linker::new();
        let linker_output = linker.run(&args).unwrap();
        assert!(
            DIRECT_COPY_SUCCESSES.load(std::sync::atomic::Ordering::Relaxed) > direct_copies_before,
            "the linker did not activate direct section copying"
        );

        let output = std::fs::read(output_path).unwrap();
        let output = object::File::parse(output.as_slice()).unwrap();
        let output_symbol = output.symbol_by_name("reld_direct_copy_payload").unwrap();
        let output_section = output
            .section_by_index(output_symbol.section_index().unwrap())
            .unwrap();
        let offset = (output_symbol.address() - output_section.address()) as usize;
        assert_eq!(
            &output_section.data().unwrap()[offset..offset + payload.len()],
            payload
        );
        drop(linker_output);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const DIRECT_TEST_SECTION_SIZE: usize = 1024 * 1024;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_output_directly_copies_an_exact_file_range() {
        let _direct_copy_guard = DIRECT_COPY_TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let input_path = directory.path().join("input.bin");
        let output_path = directory.path().join("output.bin");
        std::fs::write(&input_path, b"prefix-DIRECT-COPY-suffix").unwrap();
        let input = File::open(input_path).unwrap();
        let mut output = OsFileSystem
            .create_output(
                output_path.into(),
                OutputOptions {
                    size: 32,
                    file_replacement_mode: FileReplacementMode::UnlinkAndReplace,
                    write_mode: Some(FileWriteMode::Mmap),
                },
            )
            .unwrap();

        assert!(output.copy_file_range(&input, 7, 5, 11).unwrap());
        assert_eq!(&output.bytes()[5..16], b"DIRECT-COPY");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_or_failed_native_copy_requires_userspace_fallback() {
        let mut calls = 0;
        let complete = complete_file_range_copy(16, |remaining| {
            calls += 1;
            if calls == 1 {
                Ok(remaining / 2)
            } else {
                Err(nix::errno::Errno::EXDEV)
            }
        });

        assert!(!complete);
        assert_eq!(calls, 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_large_linux_ext4_outputs_are_preallocated() {
        assert_eq!(
            default_file_write_mode_for_filesystem_type(
                Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
                256 * 1024 * 1024,
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
        assert!(should_preallocate_large_ext4_output_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            256 * 1024 * 1024,
        ));
        assert!(!should_preallocate_large_ext4_output_for_filesystem_type(
            Some(nix::sys::statfs::EXT4_SUPER_MAGIC),
            64 * 1024 * 1024,
        ));
        assert!(!should_preallocate_large_ext4_output_for_filesystem_type(
            None,
            256 * 1024 * 1024,
        ));
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
