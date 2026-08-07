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
    /// The human-readable format label used in error messages.
    fn format_label(self) -> &'static str {
        match self {
            BridgeTarget::Coff => "COFF",
            BridgeTarget::MachO => "Mach-O",
        }
    }
}

/// A bundled bridge engine: a name, the object format it links, and how to invoke it.
///
/// This is a minimal capability seed (issue #34 / B8a) -- just enough to let `select_engine`
/// validate an explicit override against the format it's being asked to link. It is deliberately
/// not an elaborate capability matrix (LTO/GC-sections/etc.); that's later-slice scope per
/// `agents/docs/polylinker.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    /// The engine name, as accepted by `--engine=<name>` / `RELD_ENGINE`.
    name: &'static str,
    /// The object format this engine links.
    format: BridgeTarget,
    /// The file name (without extension) of the concrete linker driver this engine bridges to.
    linker_basename: &'static str,
    /// The `-flavor` value to pass to the multi-flavor `rust-lld` dispatcher to select this
    /// engine's driver.
    rust_lld_flavor: &'static str,
}

/// The bundled bridge engines. `lld-link` bridges COFF; `ld64.lld` bridges Mach-O.
const ENGINES: &[Engine] = &[
    Engine {
        name: "lld-link",
        format: BridgeTarget::Coff,
        linker_basename: "lld-link",
        rust_lld_flavor: "link",
    },
    Engine {
        name: "ld64.lld",
        format: BridgeTarget::MachO,
        linker_basename: "ld64.lld",
        rust_lld_flavor: "darwin",
    },
];

impl Engine {
    /// Looks up a bundled engine by name.
    fn find(name: &str) -> Option<&'static Engine> {
        ENGINES.iter().find(|engine| engine.name == name)
    }

    /// The default engine for a given target format (today's fixed platform->engine mapping).
    fn default_for(target: BridgeTarget) -> &'static Engine {
        ENGINES
            .iter()
            .find(|engine| engine.format == target)
            .expect("every BridgeTarget has a default engine in ENGINES")
    }
}

/// Where an engine selection came from, for the observable routing note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionReason {
    Default,
    OverrideFlag,
    OverrideEnv,
}

impl SelectionReason {
    fn label(self) -> &'static str {
        match self {
            SelectionReason::Default => "default",
            SelectionReason::OverrideFlag => "override(--engine)",
            SelectionReason::OverrideEnv => "override(RELD_ENGINE)",
        }
    }
}

/// The reld-specific argv flag that explicitly selects a bridge engine, stripped from the
/// forwarded argv before it reaches the child linker.
const ENGINE_FLAG_PREFIX: &str = "--engine=";

/// The environment variable that explicitly selects a bridge engine, checked when no `--engine=`
/// argv token is present.
pub const RELD_ENGINE_ENV: &str = "RELD_ENGINE";

/// Selects the bridge engine to use for `target`, honoring an explicit override name if given.
///
/// - `override_name` is `None`: returns the target's default engine (today's behavior).
/// - `override_name` is `Some(name)`: looks the engine up by name. An unknown name is a hard
///   error listing the valid engine names; a known engine whose format doesn't match `target` is
///   a hard error naming both the engine and the requested format.
fn select_engine(target: BridgeTarget, override_name: Option<&str>) -> Result<&'static Engine> {
    let Some(name) = override_name else {
        return Ok(Engine::default_for(target));
    };

    let Some(engine) = Engine::find(name) else {
        let valid = ENGINES
            .iter()
            .map(|engine| engine.name)
            .collect::<Vec<_>>()
            .join(", ");
        bail!("Unknown engine `{name}`. Valid engines: {valid}.");
    };

    if engine.format != target {
        bail!(
            "Engine `{name}` links {} but this link is for {}. Choose an engine that supports {}.",
            engine.format.format_label(),
            target.format_label(),
            target.format_label(),
        );
    }

    Ok(engine)
}

/// Locates the linker binary that the bridge should delegate to for the given engine.
///
/// Precedence:
/// 1. `RELD_BRIDGE_LINKER` env var, used verbatim. Errors if the path doesn't exist.
/// 2. `rust-lld` next to the active toolchain (`rustc --print sysroot`).
/// 3. `gcc-ld/<basename>` under that same rustlib bin dir, then `<basename>` on `PATH`.
/// 4. Otherwise, a hard error naming `RELD_BRIDGE_LINKER` and what to install.
pub fn discover_linker(engine: &Engine) -> Result<PathBuf> {
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

    if let Some(path) = find_concrete_linker(engine) {
        return Ok(path);
    }

    bail!("{}", not_found_message(engine));
}

/// The error message shown when no format-capable linker can be discovered. Factored out so a
/// test can assert it names `RELD_BRIDGE_LINKER` without having to defeat real toolchain
/// discovery.
fn not_found_message(engine: &Engine) -> String {
    format!(
        "Could not find a {}-capable linker to bridge to. Install a Rust toolchain (which \
         provides `rust-lld`), put `{}` on PATH, or set {RELD_BRIDGE_LINKER_ENV} to the \
         path of a linker to use.",
        engine.format.format_label(),
        engine.linker_basename,
    )
}

/// Executable suffix for the current host.
const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

/// Attempts to find `rust-lld` next to the currently active toolchain.
fn find_rust_lld() -> Option<PathBuf> {
    let candidate = rustlib_bin_dir()?.join(format!("rust-lld{EXE_SUFFIX}"));
    candidate.exists().then_some(candidate)
}

/// Attempts to find the engine's concrete linker driver: first under the active toolchain's
/// `gcc-ld` directory, then on `PATH`.
fn find_concrete_linker(engine: &Engine) -> Option<PathBuf> {
    let file_name = format!("{}{EXE_SUFFIX}", engine.linker_basename);

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
///
/// Also strips any `--engine=<name>` token, wherever it appears: that's a reld-specific flag
/// (see `select_engine`), not a linker flag, so it must never leak into the child linker's
/// command line.
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
    rest.retain(|arg| {
        arg.to_str()
            .is_none_or(|s| !s.starts_with(ENGINE_FLAG_PREFIX))
    });
    rest
}

/// Extracts the `--engine=<name>` override from the process argv, if present. Skips `argv[0]`
/// (the program path) to mirror `forwarded_args`, so a program invoked from a path that happens
/// to start with `--engine=` can never be misread as an override.
fn engine_override_from_argv<'a, I: IntoIterator<Item = &'a OsString>>(argv: I) -> Option<String> {
    argv.into_iter().skip(1).find_map(|arg| {
        arg.to_str()
            .and_then(|s| s.strip_prefix(ENGINE_FLAG_PREFIX))
            .map(str::to_owned)
    })
}

/// Builds the full command line for the child linker: the forwarded args, prefixed with
/// `-flavor <target>` when the discovered linker is the multi-flavor `rust-lld` dispatcher.
fn child_command_line(linker: &Path, engine: &Engine, forwarded: Vec<OsString>) -> Vec<OsString> {
    let mut args = Vec::with_capacity(forwarded.len() + 2);
    if needs_flavor_prefix(linker) {
        args.push(OsString::from("-flavor"));
        args.push(OsString::from(engine.rust_lld_flavor));
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
    let argv: Vec<OsString> = argv.into_iter().collect();

    let (override_name, reason) = match engine_override_from_argv(&argv) {
        Some(name) => (Some(name), SelectionReason::OverrideFlag),
        None => match std::env::var(RELD_ENGINE_ENV) {
            Ok(name) => (Some(name), SelectionReason::OverrideEnv),
            Err(_) => (None, SelectionReason::Default),
        },
    };

    let engine = select_engine(target, override_name.as_deref())?;
    let linker = discover_linker(engine)?;

    let forwarded = forwarded_args(argv);
    let child_args = child_command_line(&linker, engine, forwarded);

    eprintln!(
        "reld: engine={} (bridge, reason={}) -> {}",
        engine.name,
        reason.label(),
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

        let discovered = discover_linker(Engine::default_for(BridgeTarget::Coff)).unwrap();
        assert_eq!(discovered, fake_linker.path());

        let discovered = discover_linker(Engine::default_for(BridgeTarget::MachO)).unwrap();
        assert_eq!(discovered, fake_linker.path());
    }

    #[test]
    fn env_override_with_nonexistent_path_errors() {
        let _lock = ENV_LOCK.lock().unwrap();
        let missing = unique_temp_path("does-not-exist");
        let _guard = EnvVarGuard::set(RELD_BRIDGE_LINKER_ENV, missing.to_str().unwrap());

        let err = discover_linker(Engine::default_for(BridgeTarget::Coff)).unwrap_err();
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
            let message = not_found_message(Engine::default_for(target));
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
            Engine::default_for(BridgeTarget::Coff),
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
            Engine::default_for(BridgeTarget::MachO),
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
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::default_for(BridgeTarget::Coff),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("link"));
    }

    #[test]
    fn macho_flavor_prefix_is_darwin() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::default_for(BridgeTarget::MachO),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("darwin"));
    }

    #[test]
    fn lld_link_route_has_no_flavor_prefix() {
        let forwarded = vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")];
        let expected = forwarded.clone();
        let child = child_command_line(
            Path::new("/tc/gcc-ld/lld-link.exe"),
            Engine::default_for(BridgeTarget::Coff),
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
            Engine::default_for(BridgeTarget::MachO),
            forwarded,
        );
        assert_eq!(child, expected);
    }

    #[test]
    fn select_engine_default_for_coff_is_lld_link() {
        let engine = select_engine(BridgeTarget::Coff, None).unwrap();
        assert_eq!(engine.name, "lld-link");
        assert_eq!(engine.format, BridgeTarget::Coff);
    }

    #[test]
    fn select_engine_default_for_macho_is_ld64_lld() {
        let engine = select_engine(BridgeTarget::MachO, None).unwrap();
        assert_eq!(engine.name, "ld64.lld");
        assert_eq!(engine.format, BridgeTarget::MachO);
    }

    #[test]
    fn select_engine_valid_override_matches_target() {
        let engine = select_engine(BridgeTarget::Coff, Some("lld-link")).unwrap();
        assert_eq!(engine.name, "lld-link");

        let engine = select_engine(BridgeTarget::MachO, Some("ld64.lld")).unwrap();
        assert_eq!(engine.name, "ld64.lld");
    }

    #[test]
    fn select_engine_unknown_name_errors_listing_valid_engines() {
        let err = select_engine(BridgeTarget::Coff, Some("bogus")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("bogus"), "unexpected message: {message}");
        assert!(
            message.contains("lld-link"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("ld64.lld"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn select_engine_format_mismatch_errors() {
        // ld64.lld links Mach-O; requesting it for a COFF link is a format mismatch.
        let err = select_engine(BridgeTarget::Coff, Some("ld64.lld")).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("ld64.lld"),
            "unexpected message: {message}"
        );
        assert!(message.contains("COFF"), "unexpected message: {message}");

        // Symmetric direction: lld-link links COFF; requesting it for a Mach-O link mismatches.
        let err = select_engine(BridgeTarget::MachO, Some("lld-link")).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("lld-link"),
            "unexpected message: {message}"
        );
        assert!(message.contains("Mach-O"), "unexpected message: {message}");
    }

    #[test]
    fn forwarded_args_strips_engine_flag_at_start() {
        let argv = [
            OsString::from("reld"),
            OsString::from("--engine=lld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_engine_flag_in_middle() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("--engine=ld64.lld"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_engine_flag_at_end() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
            OsString::from("--engine=lld-link"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_unchanged_when_no_engine_flag() {
        let argv = [
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn forwarded_args_strips_both_flavor_pair_and_engine_flag() {
        let argv = [
            OsString::from("reld"),
            OsString::from("-flavor"),
            OsString::from("link"),
            OsString::from("--engine=lld-link"),
            OsString::from("/OUT:a.exe"),
            OsString::from("foo.obj"),
        ];
        assert_eq!(
            forwarded_args(argv),
            vec![OsString::from("/OUT:a.exe"), OsString::from("foo.obj")]
        );
    }

    #[test]
    fn engine_override_from_argv_finds_token_anywhere() {
        let argv = vec![
            OsString::from("reld"),
            OsString::from("/OUT:a.exe"),
            OsString::from("--engine=ld64.lld"),
        ];
        assert_eq!(
            engine_override_from_argv(&argv),
            Some("ld64.lld".to_string())
        );
    }

    #[test]
    fn engine_override_from_argv_none_when_absent() {
        let argv = vec![OsString::from("reld"), OsString::from("/OUT:a.exe")];
        assert_eq!(engine_override_from_argv(&argv), None);
    }
}
