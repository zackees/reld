//! Windows/COFF and macOS/Mach-O bridges (issue #17; BR-1 for COFF, BR-3 for Mach-O).
//!
//! `Args::new` reuses the ELF/GNU parser for `Args::Coff` because reld doesn't yet have a native
//! COFF backend. Rather than feeding MSVC-style link arguments (`/OUT:`, `/DEFAULTLIB:`, …) into
//! that parser, we bypass parsing entirely for COFF targets and delegate the raw argv straight
//! through to `lld-link` (the COFF driver built into `rust-lld`, which ships with every Rust
//! toolchain). See issue #17 for the full design rationale and issue #18 for this phase's scope.
//!
//! BR-3 (issue #29) generalizes this module to also bridge Mach-O links to `ld64.lld` (the
//! Mach-O driver in `rust-lld`), following the exact same discovery + flavor-prefix strategy,
//! selected by the `BridgeTarget` passed into `run_bridge`/`discover_linker`.
//!
//! This module intentionally never falls back to the closed-source MSVC `link.exe` (or to Apple's
//! `ld64`) -- silently doing so would poison benchmark comparability and mask discovery bugs
//! (issue #17, decision B2).

use crate::bail;
use crate::error::Context;
use crate::error::Result;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

/// Name of the environment variable that overrides linker discovery. If set, its value is used
/// verbatim as the path to the format-capable linker to bridge to.
pub const RELD_BRIDGE_LINKER_ENV: &str = "RELD_BRIDGE_LINKER";

/// Which object format the bridge should delegate links for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeTarget {
    /// Windows PE/COFF, bridged to `lld-link`.
    Coff,
    /// macOS Mach-O, bridged to `ld64.lld`.
    MachO,
}

impl BridgeTarget {
    /// The file name (without extension) of the concrete linker driver this target bridges to.
    fn linker_basename(self) -> &'static str {
        match self {
            BridgeTarget::Coff => "lld-link",
            BridgeTarget::MachO => "ld64.lld",
        }
    }

    /// The `-flavor` value to pass to the multi-flavor `rust-lld` dispatcher to select this
    /// target's driver.
    fn rust_lld_flavor(self) -> &'static str {
        match self {
            BridgeTarget::Coff => "link",
            BridgeTarget::MachO => "darwin",
        }
    }

    /// The human-readable engine label used in the `reld: engine=...` progress note.
    fn engine_label(self) -> &'static str {
        match self {
            BridgeTarget::Coff => "lld-link",
            BridgeTarget::MachO => "ld64.lld",
        }
    }
}

/// Locates the linker binary that the bridge should delegate to for the given target format.
///
/// Precedence:
/// 1. `RELD_BRIDGE_LINKER` env var, used verbatim. Errors if the path doesn't exist.
/// 2. `rust-lld` next to the active toolchain (`rustc --print sysroot`).
/// 3. `gcc-ld/<basename>` under that same rustlib bin dir, then `<basename>` on `PATH`.
/// 4. Otherwise, a hard error naming `RELD_BRIDGE_LINKER` and what to install.
pub fn discover_linker(target: BridgeTarget) -> Result<PathBuf> {
    if let Ok(value) = std::env::var(RELD_BRIDGE_LINKER_ENV) {
        let path = PathBuf::from(value);
        if !path.exists() {
            bail!(
                "{RELD_BRIDGE_LINKER_ENV} is set to `{}`, but that path does not exist",
                path.display()
            );
        }
        return Ok(path);
    }

    if let Some(path) = find_rust_lld() {
        return Ok(path);
    }

    if let Some(path) = find_concrete_linker(target) {
        return Ok(path);
    }

    bail!("{}", not_found_message(target));
}

/// The error message shown when no format-capable linker can be discovered. Factored out so a
/// test can assert it names `RELD_BRIDGE_LINKER` without having to defeat real toolchain
/// discovery.
fn not_found_message(target: BridgeTarget) -> String {
    let basename = target.linker_basename();
    format!(
        "Could not find a {}-capable linker to bridge to. Install a Rust toolchain (which \
         provides `rust-lld`), put `{basename}` on PATH, or set {RELD_BRIDGE_LINKER_ENV} to the \
         path of a linker to use.",
        match target {
            BridgeTarget::Coff => "COFF",
            BridgeTarget::MachO => "Mach-O",
        }
    )
}

/// Executable suffix for the current host.
const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

/// Attempts to find `rust-lld` next to the currently active toolchain.
fn find_rust_lld() -> Option<PathBuf> {
    let candidate = rustlib_bin_dir()?.join(format!("rust-lld{EXE_SUFFIX}"));
    candidate.exists().then_some(candidate)
}

/// Attempts to find the target's concrete linker driver: first under the active toolchain's
/// `gcc-ld` directory, then on `PATH`.
fn find_concrete_linker(target: BridgeTarget) -> Option<PathBuf> {
    let file_name = format!("{}{EXE_SUFFIX}", target.linker_basename());

    if let Some(bin) = rustlib_bin_dir() {
        let candidate = bin.join("gcc-ld").join(&file_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    find_on_path(&file_name)
}

/// The `lib/rustlib/<host-triple>/bin` directory of the active toolchain, discovered via `rustc
/// --print sysroot`. Off the hot path (discovery runs once per link), so shelling out is fine.
fn rustlib_bin_dir() -> Option<PathBuf> {
    let output = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let sysroot = String::from_utf8(output.stdout).ok()?;
    let sysroot = sysroot.trim();
    if sysroot.is_empty() {
        return None;
    }

    let host_triple = host_triple()?;

    Some(
        Path::new(sysroot)
            .join("lib")
            .join("rustlib")
            .join(host_triple)
            .join("bin"),
    )
}

/// Searches `PATH` for an executable with the given file name.
fn find_on_path(file_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(file_name);
        candidate.is_file().then_some(candidate)
    })
}

/// Returns the host target triple, e.g. `x86_64-pc-windows-msvc`, by parsing the `host:` line of
/// `rustc -vV`. (A compile-time `TARGET` is deliberately not used: cargo only sets it in a build
/// script's own environment, never in this crate's, and a stray ambient `TARGET` from a
/// cross-compile wrapper could otherwise leak in and give the wrong triple.)
fn host_triple() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
}

/// Whether the discovered linker needs an explicit `-flavor <target>` prefix to select the right
/// driver. `rust-lld` is a multi-flavor dispatcher, so it needs telling; the concrete driver
/// (`lld-link`, `ld64.lld`) is already the right driver by name, so it doesn't.
fn needs_flavor_prefix(linker: &Path) -> bool {
    linker
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("rust-lld"))
}

/// Computes the arguments to forward to the child linker from the full process argv.
///
/// Drops `argv[0]`, and also drops a leading `-flavor <name>` pair if present: when reld is
/// invoked via the `-flavor link`/`-flavor darwin` multi-call convention (see `Args::new`), that
/// selector has already been consumed by reld's own platform dispatch, so forwarding it would
/// leak a stray `-flavor` into the child linker (and, for `rust-lld`, collide with the `-flavor
/// <target>` that `child_command_line` prepends).
fn forwarded_args<I: IntoIterator<Item = OsString>>(argv: I) -> Vec<OsString> {
    let mut rest: Vec<OsString> = argv.into_iter().skip(1).collect();
    if rest
        .first()
        .is_some_and(|arg| arg.as_os_str() == OsStr::new("-flavor"))
    {
        // Drop `-flavor` and its argument (or just `-flavor` if it was the trailing token).
        let drop = rest.len().min(2);
        rest.drain(0..drop);
    }
    rest
}

/// Builds the full command line for the child linker: the forwarded args, prefixed with
/// `-flavor <target>` when the discovered linker is the multi-flavor `rust-lld` dispatcher.
fn child_command_line(
    linker: &Path,
    target: BridgeTarget,
    forwarded: Vec<OsString>,
) -> Vec<OsString> {
    let mut args = Vec::with_capacity(forwarded.len() + 2);
    if needs_flavor_prefix(linker) {
        args.push(OsString::from("-flavor"));
        args.push(OsString::from(target.rust_lld_flavor()));
    }
    args.extend(forwarded);
    args
}

/// Runs the bridge: discovers a linker capable of the given target format and execs it with the
/// pass-through argv, bypassing reld's own argument parser entirely.
///
/// `argv` is the full process argv, including `argv[0]`; `argv[0]` is dropped and the rest is
/// forwarded as the linker's command line (with `-flavor <target>` prepended if the discovered
/// linker needs it).
pub fn run_bridge<I: IntoIterator<Item = OsString>>(argv: I, target: BridgeTarget) -> Result<()> {
    let linker = discover_linker(target)?;

    let forwarded = forwarded_args(argv);
    let child_args = child_command_line(&linker, target, forwarded);

    eprintln!(
        "reld: engine={} (bridge) -> {}",
        target.engine_label(),
        linker.display()
    );

    let mut command = std::process::Command::new(&linker);
    command.args(child_args);

    command.stdin(std::process::Stdio::inherit());
    command.stdout(std::process::Stdio::inherit());
    command.stderr(std::process::Stdio::inherit());

    let status = command
        .status()
        .with_context(|| format!("Failed to spawn bridge linker `{}`", linker.display()))?;

    if !status.success() {
        // Match how reld currently exits (see `report_error_and_exit`): terminate the process
        // directly with the child's exit code, rather than propagating a `Result` error that
        // would go through our own error formatting.
        let code = status.code().unwrap_or(1);
        std::process::exit(code);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Environment variable mutation isn't thread-safe, so serialize the tests that touch
    // `RELD_BRIDGE_LINKER`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: Tests are serialized via ENV_LOCK, so no other thread observes this
            // process's environment concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: See above.
            unsafe { std::env::remove_var(self.key) };
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "reld-bridge-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        path
    }

    /// Creates an empty file at a unique temp path. Returns a guard that removes it on drop.
    struct TempFile(PathBuf);

    impl TempFile {
        fn create(name: &str) -> Self {
            let path = unique_temp_path(name);
            std::fs::write(&path, b"").unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn env_override_returns_set_path() {
        let _lock = ENV_LOCK.lock().unwrap();
        let fake_linker = TempFile::create("env-override-linker");
        let _guard = EnvVarGuard::set(RELD_BRIDGE_LINKER_ENV, fake_linker.path().to_str().unwrap());

        let discovered = discover_linker(BridgeTarget::Coff).unwrap();
        assert_eq!(discovered, fake_linker.path());

        let discovered = discover_linker(BridgeTarget::MachO).unwrap();
        assert_eq!(discovered, fake_linker.path());
    }

    #[test]
    fn env_override_with_nonexistent_path_errors() {
        let _lock = ENV_LOCK.lock().unwrap();
        let missing = unique_temp_path("does-not-exist");
        let _guard = EnvVarGuard::set(RELD_BRIDGE_LINKER_ENV, missing.to_str().unwrap());

        let err = discover_linker(BridgeTarget::Coff).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(RELD_BRIDGE_LINKER_ENV),
            "unexpected error message: {message}"
        );
        assert!(
            message.contains("does not exist"),
            "unexpected error message: {message}"
        );
    }

    #[test]
    fn needs_flavor_prefix_for_rust_lld() {
        // Forward-slash paths so `file_stem()` behaves identically on every host (backslashes
        // are not path separators off Windows). The backslash form is covered under cfg(windows).
        assert!(needs_flavor_prefix(Path::new("/some/path/rust-lld")));
        assert!(needs_flavor_prefix(Path::new("/some/path/rust-lld.exe")));
        assert!(needs_flavor_prefix(Path::new("RUST-LLD")));
        #[cfg(windows)]
        assert!(needs_flavor_prefix(Path::new(r"C:\some\path\rust-lld.exe")));
    }

    #[test]
    fn no_flavor_prefix_for_concrete_drivers() {
        assert!(!needs_flavor_prefix(Path::new("/some/path/lld-link")));
        assert!(!needs_flavor_prefix(Path::new("/some/path/lld-link.exe")));
        assert!(!needs_flavor_prefix(Path::new("/some/path/ld64.lld")));
        #[cfg(windows)]
        assert!(!needs_flavor_prefix(Path::new(
            r"C:\some\path\lld-link.exe"
        )));
    }

    #[test]
    fn not_found_message_names_env_var() {
        // The "no linker discoverable" error must always tell the user about the override knob,
        // for both bridge targets.
        for target in [BridgeTarget::Coff, BridgeTarget::MachO] {
            let message = not_found_message(target);
            assert!(
                message.contains(RELD_BRIDGE_LINKER_ENV),
                "unexpected error message: {message}"
            );
        }
    }

    #[test]
    fn forwarded_args_drops_only_argv0_for_name_route() {
        let argv = [
            OsString::from("reld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_leading_flavor_pair() {
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn flavor_route_to_rust_lld_yields_exactly_one_flavor_pair_coff() {
        // The bug this guards against: a `-flavor link` invocation of reld, bridged to the
        // multi-flavor `rust-lld`, must produce a single `-flavor link` — never a doubled pair
        // that leaks into the COFF driver.
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("/OUT:a.exe"),
        ];
        let child = child_command_line(
            Path::new("/tc/rust-lld.exe"),
            BridgeTarget::Coff,
            forwarded_args(argv),
        );
        assert_eq!(
            child,
            vec![
                OsString::from("-flavor"),
                OsString::from("link"),
                OsString::from("/OUT:a.exe"),
            ]
        );
    }

    #[test]
    fn flavor_route_to_rust_lld_yields_exactly_one_flavor_pair_macho() {
        // Same guard as above, but for the Mach-O target: a `-flavor darwin` invocation of reld,
        // bridged to the multi-flavor `rust-lld`, must produce a single `-flavor darwin`.
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("darwin"),
            OsString::from("-o"),
            OsString::from("a.out"),
        ];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            BridgeTarget::MachO,
            forwarded_args(argv),
        );
        assert_eq!(
            child,
            vec![
                OsString::from("-flavor"),
                OsString::from("darwin"),
                OsString::from("-o"),
                OsString::from("a.out"),
            ]
        );
    }

    #[test]
    fn coff_flavor_prefix_is_link() {
        let forwarded = vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")];
        let child = child_command_line(Path::new("/tc/rust-lld"), BridgeTarget::Coff, forwarded);
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("link"));
    }

    #[test]
    fn macho_flavor_prefix_is_darwin() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let child = child_command_line(Path::new("/tc/rust-lld"), BridgeTarget::MachO, forwarded);
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("darwin"));
    }

    #[test]
    fn lld_link_route_has_no_flavor_prefix() {
        let forwarded = vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")];
        let expected = forwarded.clone();
        let child = child_command_line(
            Path::new("/tc/gcc-ld/lld-link.exe"),
            BridgeTarget::Coff,
            forwarded,
        );
        assert_eq!(child, expected);
    }

    #[test]
    fn ld64_lld_route_has_no_flavor_prefix() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let expected = forwarded.clone();
        let child = child_command_line(
            Path::new("/tc/gcc-ld/ld64.lld"),
            BridgeTarget::MachO,
            forwarded,
        );
        assert_eq!(child, expected);
    }
}
