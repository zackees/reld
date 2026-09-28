//! Fallback host library search directories for Linux ELF links.
//!
//! `-l<name>` arguments normally resolve against `-L` directories the driver passes explicitly.
//! On NixOS and other non-FHS hosts, the C runtime and `libgcc_s` live under content-addressed
//! `/nix/store/...` paths that only the host's own wrapped C compiler knows about. When reld is
//! invoked directly with an *unwrapped* compiler (e.g. `clang --ld-path=reld` using a
//! standalone clang, bypassing Nix's `cc` wrapper), no `-L` names those directories and the link
//! fails with "Couldn't find library".
//!
//! This module recovers those directories the way any linker driver would: by asking the host's
//! own C compiler where it thinks its libraries live (`cc -print-search-dirs`), and by honouring
//! the same environment variables a wrapped compiler uses to pass search directories along
//! (`NIX_LDFLAGS`'s `-L` entries, and the GCC/Clang `LIBRARY_PATH` convention). The result is
//! computed at most once per process, and it is only consulted after the explicit `-L` search
//! path has already missed, so a normal link that resolves everything through `-L` never spawns a
//! subprocess.
//!
//! Linux/ELF only: this answers a *host* library-resolution question (what does the machine
//! running reld provide?), not a linker-target one, so it is gated on the host being Linux.

use crate::platforms::host::HostOs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Directories to fall back to when a `-l<name>` library isn't found on the explicit search
/// path. Empty on non-Linux hosts. Cheap after the first call: the result is cached for the rest
/// of the process.
pub(crate) fn fallback_library_dirs() -> Vec<PathBuf> {
    let mut guard = cache().lock().unwrap();
    if let Some(dirs) = guard.as_ref() {
        return dirs.clone();
    }
    let dirs = compute_fallback_library_dirs();
    *guard = Some(dirs.clone());
    dirs
}

/// Clears the cache so a test can observe a fresh computation under a different environment.
/// Tests that use this must serialize with any other test that touches the same environment
/// variables (see `ENV_LOCK` below).
#[cfg(test)]
pub(crate) fn reset_cache_for_test() {
    *cache().lock().unwrap() = None;
}

fn cache() -> &'static Mutex<Option<Vec<PathBuf>>> {
    static CACHE: OnceLock<Mutex<Option<Vec<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn compute_fallback_library_dirs() -> Vec<PathBuf> {
    if !matches!(crate::platforms::host::os(), HostOs::Linux) {
        return Vec::new();
    }
    let nix_ldflags = crate::env::var("NIX_LDFLAGS").ok();
    let library_path = crate::env::var("LIBRARY_PATH").ok();
    let compiler_dirs = discover_compiler_search_dirs();
    merge_fallback_dirs(
        nix_ldflags.as_deref(),
        library_path.as_deref(),
        compiler_dirs,
    )
}

/// Combines the three fallback sources into a deduplicated list of directories that exist.
///
/// Order: `NIX_LDFLAGS`'s `-L` entries first (most specific to the current build environment),
/// then `LIBRARY_PATH` (the GCC/Clang extra-search-dir convention), then the host compiler's
/// reported default library directories (broadest, and the only source that needs a subprocess).
fn merge_fallback_dirs(
    nix_ldflags: Option<&str>,
    library_path: Option<&str>,
    compiler_dirs: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut seen = hashbrown::HashSet::new();
    let mut dirs = Vec::new();
    let push = |dir: PathBuf, dirs: &mut Vec<PathBuf>, seen: &mut hashbrown::HashSet<PathBuf>| {
        if dir.is_dir() && seen.insert(dir.clone()) {
            dirs.push(dir);
        }
    };
    if let Some(flags) = nix_ldflags {
        for dir in parse_ldflags_library_dirs(flags) {
            push(dir, &mut dirs, &mut seen);
        }
    }
    if let Some(value) = library_path {
        for dir in std::env::split_paths(value) {
            push(dir, &mut dirs, &mut seen);
        }
    }
    for dir in compiler_dirs {
        push(dir, &mut dirs, &mut seen);
    }
    dirs
}

/// Parses `-L<dir>` / `-L <dir>` entries out of a `NIX_LDFLAGS`-style whitespace-separated flag
/// string. Other flags (`-B<dir>`, `-rpath`, ...) are ignored.
fn parse_ldflags_library_dirs(flags: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut tokens = flags.split_whitespace();
    while let Some(token) = tokens.next() {
        if let Some(dir) = token.strip_prefix("-L") {
            if !dir.is_empty() {
                dirs.push(PathBuf::from(dir));
            } else if let Some(next) = tokens.next() {
                dirs.push(PathBuf::from(next));
            }
        }
    }
    dirs
}

/// Runs the host's C compiler with `-print-search-dirs` and parses its `libraries:` line.
///
/// Best-effort: any failure (no compiler on `PATH`, unexpected output, ...) yields an empty list
/// rather than an error. This is a diagnostic-recovery aid, never something that should turn into
/// a hard link failure of its own.
fn discover_compiler_search_dirs() -> Vec<PathBuf> {
    for compiler in host_compiler_candidates() {
        if let Some(output) = run_print_search_dirs(&compiler) {
            let dirs = parse_search_dirs_output(&output);
            if !dirs.is_empty() {
                return dirs;
            }
        }
    }
    Vec::new()
}

/// Compiler executables to try, in order. `CC` first, matching every other tool that lets the
/// environment pick the toolchain (this is also how a Nix build environment points at its own
/// wrapped compiler).
fn host_compiler_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(cc) = crate::env::var("CC") {
        candidates.push(cc);
    }
    for default in ["cc", "gcc", "clang"] {
        candidates.push(default.to_owned());
    }
    candidates
}

fn run_print_search_dirs(compiler: &str) -> Option<String> {
    let output = Command::new(compiler)
        .arg("-print-search-dirs")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Parses the `libraries: =dir1:dir2:...` line from `cc -print-search-dirs` output.
fn parse_search_dirs_output(output: &str) -> Vec<PathBuf> {
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("libraries:") else {
            continue;
        };
        let rest = rest.trim().trim_start_matches('=');
        return std::env::split_paths(rest).collect();
    }
    Vec::new()
}

/// Test-only helpers shared with `input_data`'s host-fallback test, so both live behind one
/// `ENV_LOCK` and never race each other's environment-variable mutations.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex as StdMutex;

    /// Serializes tests (in this crate) that mutate `LIBRARY_PATH`, `NIX_LDFLAGS` or `CC`.
    pub(crate) static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    pub(crate) struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        /// Sets `key` to `value`. Callers must hold `ENV_LOCK` for the guard's whole lifetime.
        pub(crate) fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: Callers hold `ENV_LOCK` for the guard's whole lifetime, so no other thread
            // in this process observes the mutation concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: See `set`.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::ENV_LOCK;
    use super::test_support::EnvVarGuard;
    use super::*;

    #[test]
    fn parses_ldflags_library_dirs() {
        let flags = "-L/nix/store/aaa-glibc/lib -B/nix/store/bbb -L /nix/store/ccc/lib -rpath /x";
        let dirs = parse_ldflags_library_dirs(flags);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/nix/store/aaa-glibc/lib"),
                PathBuf::from("/nix/store/ccc/lib"),
            ]
        );
    }

    #[test]
    fn parses_print_search_dirs_libraries_line() {
        let output = "install: /usr/lib/gcc/x86_64-linux-gnu/12/\n\
                       programs: =/a:/b\n\
                       libraries: =/a/lib:/b/lib:/c/lib\n";
        let dirs = parse_search_dirs_output(output);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/a/lib"),
                PathBuf::from("/b/lib"),
                PathBuf::from("/c/lib"),
            ]
        );
    }

    #[test]
    fn parse_search_dirs_output_missing_line_is_empty() {
        assert_eq!(
            parse_search_dirs_output("install: /foo\n"),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn merges_and_dedupes_existing_dirs_in_source_order() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let missing = tmp.path().join("missing");

        let ldflags = format!("-L{} -L{}", a.display(), a.display());
        let library_path = format!("{}:{}", missing.display(), b.display());
        let compiler_dirs = vec![a.clone(), b.clone()];

        let dirs = merge_fallback_dirs(Some(&ldflags), Some(&library_path), compiler_dirs);
        assert_eq!(dirs, vec![a, b]);
    }

    /// RED before the fix: a library only reachable via the `LIBRARY_PATH` fallback source is
    /// invisible to `fallback_library_dirs` unless the host is treated as Linux and the
    /// environment variable is actually consulted. GREEN after `compute_fallback_library_dirs`
    /// wires `LIBRARY_PATH` in.
    #[test]
    fn fallback_library_dirs_honours_library_path_on_linux_host() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        if !matches!(crate::platforms::host::os(), HostOs::Linux) {
            // This fallback is Linux-only by design; nothing to assert elsewhere.
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let lib_dir = tmp.path().join("only-via-library-path");
        std::fs::create_dir(&lib_dir).unwrap();
        std::fs::write(lib_dir.join("libgcc_s_fallback_test.so"), b"").unwrap();

        let _library_path = EnvVarGuard::set("LIBRARY_PATH", &lib_dir.display().to_string());
        let _nix_ldflags = EnvVarGuard::set("NIX_LDFLAGS", "");
        // Force the compiler-query source to fail deterministically regardless of what's on the
        // test host's `PATH`, so this test exercises exactly the `LIBRARY_PATH` source.
        let _cc = EnvVarGuard::set("CC", "reld-host-lib-search-test-nonexistent-compiler");
        reset_cache_for_test();

        let dirs = fallback_library_dirs();
        reset_cache_for_test();

        assert!(
            dirs.contains(&lib_dir),
            "expected {lib_dir:?} in fallback dirs, got {dirs:?}"
        );
    }
}
