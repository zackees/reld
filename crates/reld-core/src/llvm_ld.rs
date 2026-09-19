//! In-process COFF and MinGW links through llvm-ld-coff (`zackees/llvm-ld`, reld#96).
//!
//! llvm-ld-coff is LLD's **COFF** linker, the lld-link/MSVC and MinGW drivers, built as a shared
//! library behind a narrow C ABI (`llvm_ld.h`). It only ever links Windows PE/COFF outputs. It is
//! published for Windows, Linux and macOS *hosts*, so on Linux and macOS it is a Windows
//! cross-linker. It produces the same output as stock LLD and links faster, and calling it
//! in-process also saves spawning `lld-link` for every link, which is not cheap on Windows.
//!
//! reld never compiles it. llvm-ld publishes a checksummed prebuilt per host on its GitHub
//! Releases, reld's release pins one by version and SHA-256 and ships the library next to reld,
//! and this module loads it at runtime. Updating either repository never forces a rebuild of the
//! other.
//!
//! Everything here is best-effort: if the library is absent, disabled, the wrong ABI version, or
//! busy, [`invoke`] returns [`Outcome::Unavailable`] and the bridge spawns `lld-link` as it always
//! has. That subprocess path stays the fallback and the escape hatch (`RELD_LLVM_LD=off`).

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

/// Selects the library, or disables the in-process path: a path to the shared library, or `off`.
pub const RELD_LLVM_LD_ENV: &str = "RELD_LLVM_LD";

/// The C ABI version this module was written against (`LLVM_LD_ABI_VERSION`).
const ABI_VERSION: u32 = 1;

const LLVM_LD_OK: i32 = 0;
const LLVM_LD_E_BUSY: i32 = -2;

/// Which LLD driver to run, matching `llvm_ld_driver`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Driver {
    /// `lld-link`: MSVC-style arguments.
    WinLink = 1,
    /// LLD's MinGW driver: GNU-style arguments for a PE/COFF output.
    MinGw = 2,
}

impl Driver {
    /// The program name LLD expects in `argv[0]`; it parses from `argv[1]`.
    fn program_name(self) -> &'static str {
        match self {
            Driver::WinLink => "lld-link",
            Driver::MinGw => "ld.lld",
        }
    }
}

/// What happened when a link was offered to llvm-ld.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// llvm-ld ran the link; this is the linker's exit code.
    Linked { exit_code: i32, library: PathBuf },
    /// llvm-ld could not take the link; the caller should use the subprocess bridge instead.
    Unavailable { reason: String },
}

type WriteFn = unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize);

#[repr(C)]
struct Request {
    struct_size: u32,
    abi_version: u32,
    driver: u32,
    argc: u32,
    argv: *const *const u8,
    argv_lengths: *const usize,
    stdout_write: Option<WriteFn>,
    stderr_write: Option<WriteFn>,
    callback_context: *mut std::ffi::c_void,
}

#[repr(C)]
struct LinkResult {
    struct_size: u32,
    return_code: i32,
    can_run_again: u8,
    reserved: [u8; 3],
}

unsafe extern "C" fn write_stdout(_: *mut std::ffi::c_void, bytes: *const u8, length: usize) {
    // SAFETY: llvm-ld passes a buffer of `length` bytes that is valid for this call.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
    let _ = std::io::stdout().write_all(bytes);
}

unsafe extern "C" fn write_stderr(_: *mut std::ffi::c_void, bytes: *const u8, length: usize) {
    // SAFETY: as above.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
    let _ = std::io::stderr().write_all(bytes);
}

/// Where the library would be loaded from, or why the in-process path is off.
///
/// `RELD_LLVM_LD` wins: `off`/`0`/`false` disables it, and any other value is the library's path.
/// Otherwise the library is looked for next to the running reld, which is where the release
/// archive puts it.
pub(crate) fn locate(env_value: Option<OsString>, exe: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(value) = env_value {
        let text = value.to_string_lossy();
        if matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false"
        ) {
            return Err(format!("disabled by {RELD_LLVM_LD_ENV}={text}"));
        }
        if !text.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    let dir = exe
        .and_then(Path::parent)
        .ok_or_else(|| "cannot locate the running executable".to_owned())?;
    let candidate = dir.join(libloading::library_filename("llvm_ld"));
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!("no {} next to reld", candidate.display()))
    }
}

/// The argv llvm-ld receives: the driver's program name, then the linker arguments. llvm-ld
/// requires UTF-8, so a non-UTF-8 argument makes the link ineligible rather than mangled.
pub(crate) fn request_argv(driver: Driver, args: &[OsString]) -> Option<Vec<String>> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(driver.program_name().to_owned());
    for arg in args {
        argv.push(arg.to_str()?.to_owned());
    }
    Some(argv)
}

/// Run one link through llvm-ld in-process, or report why it cannot.
pub(crate) fn invoke(driver: Driver, args: &[OsString]) -> Outcome {
    invoke_with(
        driver,
        args,
        std::env::var_os(RELD_LLVM_LD_ENV),
        std::env::current_exe().ok().as_deref(),
    )
}

fn invoke_with(
    driver: Driver,
    args: &[OsString],
    env_value: Option<OsString>,
    exe: Option<&Path>,
) -> Outcome {
    let library = match locate(env_value, exe) {
        Ok(path) => path,
        Err(reason) => return Outcome::Unavailable { reason },
    };
    let Some(argv) = request_argv(driver, args) else {
        return Outcome::Unavailable {
            reason: "an argument is not valid UTF-8".to_owned(),
        };
    };
    // SAFETY: loading a library runs its initializers. The path is either reld's own shipped
    // llvm_ld, or one the user named explicitly in RELD_LLVM_LD.
    let lib = match unsafe { libloading::Library::new(&library) } {
        Ok(lib) => lib,
        Err(error) => {
            return Outcome::Unavailable {
                reason: format!("cannot load {}: {error}", library.display()),
            };
        }
    };
    // SAFETY: the symbol types match llvm_ld.h for ABI version 1, which is checked before any
    // other call is made.
    unsafe {
        let Ok(abi_version) = lib.get::<unsafe extern "C" fn() -> u32>(b"llvm_ld_abi_version\0")
        else {
            return Outcome::Unavailable {
                reason: format!("{} does not export llvm_ld_abi_version", library.display()),
            };
        };
        let found = abi_version();
        if found != ABI_VERSION {
            return Outcome::Unavailable {
                reason: format!(
                    "{} has C ABI version {found}, reld needs {ABI_VERSION}",
                    library.display()
                ),
            };
        }
        let Ok(link) = lib.get::<unsafe extern "C" fn(*const Request, *mut LinkResult) -> i32>(
            b"llvm_ld_invoke\0",
        ) else {
            return Outcome::Unavailable {
                reason: format!("{} does not export llvm_ld_invoke", library.display()),
            };
        };

        let pointers: Vec<*const u8> = argv.iter().map(|arg| arg.as_ptr()).collect();
        let lengths: Vec<usize> = argv.iter().map(String::len).collect();
        let request = Request {
            struct_size: size_of::<Request>() as u32,
            abi_version: ABI_VERSION,
            driver: driver as u32,
            argc: argv.len() as u32,
            argv: pointers.as_ptr(),
            argv_lengths: lengths.as_ptr(),
            stdout_write: Some(write_stdout),
            stderr_write: Some(write_stderr),
            callback_context: std::ptr::null_mut(),
        };
        let mut result = LinkResult {
            struct_size: size_of::<LinkResult>() as u32,
            return_code: 0,
            can_run_again: 0,
            reserved: [0; 3],
        };
        let status = link(&raw const request, &raw mut result);
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        match status {
            LLVM_LD_OK => Outcome::Linked {
                exit_code: result.return_code,
                library,
            },
            LLVM_LD_E_BUSY => Outcome::Unavailable {
                reason: "llvm-ld is busy with another link in this process".to_owned(),
            },
            other => Outcome::Unavailable {
                reason: format!("llvm_ld_invoke returned {other}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_off_disables_the_in_process_path() {
        for value in ["off", "OFF", "0", "false", " off "] {
            let error = locate(Some(OsString::from(value)), None).unwrap_err();
            assert!(error.contains("disabled"), "{value}: {error}");
        }
    }

    #[test]
    fn env_path_is_used_verbatim() {
        let path = locate(Some(OsString::from("/opt/llvm_ld.dll")), None).unwrap();
        assert_eq!(path, PathBuf::from("/opt/llvm_ld.dll"));
    }

    #[test]
    fn default_looks_next_to_the_executable_and_reports_when_absent() {
        let dir = std::env::temp_dir().join("reld-llvm-ld-locate-test");
        let _ = std::fs::create_dir_all(&dir);
        let exe = dir.join("reld");
        let library = dir.join(libloading::library_filename("llvm_ld"));
        let _ = std::fs::remove_file(&library);
        let error = locate(None, Some(&exe)).unwrap_err();
        assert!(error.contains("next to reld"), "{error}");

        std::fs::write(&library, b"").unwrap();
        assert_eq!(locate(None, Some(&exe)).unwrap(), library);
        let _ = std::fs::remove_file(&library);
    }

    #[test]
    fn argv_zero_is_the_driver_program_name() {
        let args = [OsString::from("/out:a.exe"), OsString::from("a.obj")];
        assert_eq!(
            request_argv(Driver::WinLink, &args).unwrap(),
            ["lld-link", "/out:a.exe", "a.obj"]
        );
        assert_eq!(request_argv(Driver::MinGw, &args).unwrap()[0], "ld.lld");
    }

    #[test]
    fn a_file_that_is_not_a_library_falls_back_instead_of_failing() {
        // The bridge must be able to rely on this: anything short of a working llvm-ld is
        // `Unavailable`, never a failed link.
        let dir = std::env::temp_dir().join("reld-llvm-ld-not-a-library");
        let _ = std::fs::create_dir_all(&dir);
        let bogus = dir.join(libloading::library_filename("llvm_ld"));
        std::fs::write(&bogus, b"not a shared library").unwrap();
        let outcome = invoke_with(
            Driver::WinLink,
            &[OsString::from("/out:a.exe")],
            Some(bogus.clone().into_os_string()),
            None,
        );
        let _ = std::fs::remove_file(&bogus);
        match outcome {
            Outcome::Unavailable { reason } => assert!(reason.contains("cannot load"), "{reason}"),
            other @ Outcome::Linked { .. } => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn disabled_path_never_touches_the_filesystem() {
        let outcome = invoke_with(Driver::MinGw, &[], Some(OsString::from("off")), None);
        assert!(matches!(outcome, Outcome::Unavailable { reason } if reason.contains("disabled")));
    }
}
