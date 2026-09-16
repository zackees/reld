//! Polylinker routing and subprocess bridges (issue #17).
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
//! BR-6 adds capability-based ELF routing. Ordinary ELF links stay in reld's native engine, while
//! requests that need a capability the native engine does not implement are delegated to the ELF
//! driver in `lld`.
//!
//! This module intentionally never falls back to the closed-source MSVC `link.exe` (or to Apple's
//! `ld64`) -- silently doing so would poison benchmark comparability and mask discovery bugs
//! (issue #17, decision B2).

use crate::bail;
use crate::error::Context;
use crate::error::Result;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

/// Name of the environment variable that overrides linker discovery. If set, its value is used
/// verbatim as the path to the format-capable linker to bridge to.
pub const RELD_BRIDGE_LINKER_ENV: &str = "RELD_BRIDGE_LINKER";

/// Which object format the bridge should delegate links for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeTarget {
    /// Linux ELF, normally linked natively and bridged to `ld.lld` when capabilities require it.
    Elf,
    /// Windows PE/COFF, bridged to `lld-link`.
    Coff,
    /// macOS Mach-O, bridged to `ld64.lld`.
    MachO,
}

impl BridgeTarget {
    /// The human-readable format label used in error messages.
    fn format_label(self) -> &'static str {
        match self {
            BridgeTarget::Elf => "ELF",
            BridgeTarget::Coff => "COFF",
            BridgeTarget::MachO => "Mach-O",
        }
    }
}

/// A bundled linker engine: its public name, object format, invocation, and capabilities.
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
    /// Whether this engine runs in-process rather than through the subprocess bridge.
    native: bool,
    /// Link configurations this engine can satisfy without silently dropping their semantics.
    capabilities: &'static [Capability],
}

/// Capabilities that affect engine selection rather than merely argument spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    NativeControl,
    Lto,
    Icf,
    DiscardAll,
    FatalWarnings,
    ColorDiagnostics,
    VersionScriptPolicy,
    CortexA53Erratum,
}

impl Capability {
    fn label(self) -> &'static str {
        match self {
            Capability::NativeControl => "reld-only validation or diagnostic output",
            Capability::Lto => "LTO",
            Capability::Icf => "identical code folding",
            Capability::DiscardAll => "discarding local symbols",
            Capability::FatalWarnings => "fatal linker warnings",
            Capability::ColorDiagnostics => "diagnostic color policy",
            Capability::VersionScriptPolicy => "version-script undefined-symbol policy",
            Capability::CortexA53Erratum => "Cortex-A53 erratum 843419 fixups",
        }
    }
}

const NATIVE_RELD_CAPABILITIES: &[Capability] = &[Capability::NativeControl];
const LLD_CAPABILITIES: &[Capability] = &[
    Capability::Lto,
    Capability::Icf,
    Capability::DiscardAll,
    Capability::FatalWarnings,
    Capability::ColorDiagnostics,
    Capability::VersionScriptPolicy,
    Capability::CortexA53Erratum,
];

const NATIVE_RELD_ENGINE: Engine = Engine {
    name: "reld",
    format: BridgeTarget::Elf,
    linker_basename: "",
    rust_lld_flavor: "",
    native: true,
    capabilities: NATIVE_RELD_CAPABILITIES,
};
const ELF_LLD_ENGINE: Engine = Engine {
    name: "lld",
    format: BridgeTarget::Elf,
    linker_basename: "ld.lld",
    rust_lld_flavor: "gnu",
    native: false,
    capabilities: LLD_CAPABILITIES,
};
const COFF_LLD_ENGINE: Engine = Engine {
    name: "lld-link",
    format: BridgeTarget::Coff,
    linker_basename: "lld-link",
    rust_lld_flavor: "link",
    native: false,
    capabilities: LLD_CAPABILITIES,
};
const MACHO_LLD_ENGINE: Engine = Engine {
    name: "ld64.lld",
    format: BridgeTarget::MachO,
    linker_basename: "ld64.lld",
    rust_lld_flavor: "darwin",
    native: false,
    capabilities: LLD_CAPABILITIES,
};

/// The available engines and their capabilities. Ordering defines the default (fastest) engine
/// for each format: native reld for ELF, and the appropriate lld driver for COFF/Mach-O.
const ENGINES: &[Engine] = &[
    NATIVE_RELD_ENGINE,
    ELF_LLD_ENGINE,
    COFF_LLD_ENGINE,
    MACHO_LLD_ENGINE,
];

impl Engine {
    /// Looks up a bundled engine by name.
    fn find(name: &str) -> Option<&'static Engine> {
        ENGINES.iter().find(|engine| engine.name == name)
    }

    /// The default engine for a given target format (today's fixed platform->engine mapping).
    fn default_for(target: BridgeTarget) -> &'static Engine {
        match target {
            BridgeTarget::Elf => &NATIVE_RELD_ENGINE,
            BridgeTarget::Coff => &COFF_LLD_ENGINE,
            BridgeTarget::MachO => &MACHO_LLD_ENGINE,
        }
    }

    fn supports(self, requirements: &[Requirement]) -> bool {
        requirements
            .iter()
            .all(|requirement| self.capabilities.contains(&requirement.capability))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Requirement {
    capability: Capability,
    trigger: &'static str,
}

/// Where an engine selection came from, for the observable routing note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionReason {
    Default,
    OverrideFlag,
    OverrideEnv,
    Capability(&'static str),
}

impl SelectionReason {
    fn label(self) -> &'static str {
        match self {
            SelectionReason::Default => "default",
            SelectionReason::OverrideFlag => "override(--engine)",
            SelectionReason::OverrideEnv => "override(RELD_ENGINE)",
            SelectionReason::Capability(trigger) => trigger,
        }
    }
}

/// The reld-specific argv flag that explicitly selects a bridge engine, stripped from the
/// forwarded argv before it reaches the child linker.
const ENGINE_FLAG_PREFIX: &str = "--engine=";

/// The environment variable that explicitly selects a bridge engine, checked when no `--engine=`
/// argv token is present.
pub const RELD_ENGINE_ENV: &str = "RELD_ENGINE";

/// Enables one routing-decision line on stderr when present in the environment.
pub const RELD_LOG_ENGINE_ENV: &str = "RELD_LOG_ENGINE";

/// Appends one JSON object after every successful link when set to a file path.
///
/// This is an acceptance-test audit channel, deliberately separate from human-readable stderr
/// logging. A record is written only after the selected native or bridge engine returns success.
pub const RELD_INVOCATION_LOG_ENV: &str = "RELD_INVOCATION_LOG";

fn route_logging_enabled() -> bool {
    std::env::var_os(RELD_LOG_ENGINE_ENV).is_some()
}

/// The selected engine and the reason it was selected for one link request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    engine: &'static Engine,
    reason: SelectionReason,
}

fn decode_response_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read linker response file `{}`", path.display()))?;
    if bytes.starts_with(&[0xff, 0xfe])
        || (bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes[1] == 0)
    {
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .skip(usize::from(bytes.starts_with(&[0xff, 0xfe])))
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16(&words)
            .with_context(|| format!("Invalid UTF-16 linker response file `{}`", path.display()));
    }
    String::from_utf8(bytes)
        .with_context(|| format!("Invalid UTF-8 linker response file `{}`", path.display()))
}

fn response_arguments(contents: &str) -> Result<Vec<OsString>> {
    let mut arguments = Vec::new();
    let mut argument = String::new();
    let mut quote = None;
    for character in contents.chars() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, character) if character.is_whitespace() => {
                if !argument.is_empty() {
                    arguments.push(OsString::from(std::mem::take(&mut argument)));
                }
            }
            (_, character) => argument.push(character),
        }
    }
    if let Some(quote) = quote {
        bail!("Unclosed `{quote}` in linker response file");
    }
    if !argument.is_empty() {
        arguments.push(OsString::from(argument));
    }
    Ok(arguments)
}

fn requested_output_in(argv: &[OsString], response_depth: usize) -> Result<Option<String>> {
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        let value = argument.to_string_lossy();
        if let Some(path) = value.strip_prefix('@') {
            if response_depth >= 16 {
                bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
            }
            let nested = response_arguments(&decode_response_file(Path::new(path))?)?;
            if let Some(output) = requested_output_in(&nested, response_depth + 1)? {
                return Ok(Some(output));
            }
            continue;
        }
        if value == "-o" {
            return Ok(arguments
                .next()
                .map(|output| output.to_string_lossy().into_owned()));
        }
        let lowercase = value.to_ascii_lowercase();
        if let Some(output) = lowercase
            .strip_prefix("/out:")
            .or_else(|| lowercase.strip_prefix("-out:"))
        {
            let prefix_length = value.len() - output.len();
            return Ok(Some(value[prefix_length..].to_owned()));
        }
    }
    Ok(None)
}

fn requested_output(argv: &[OsString]) -> Result<Option<String>> {
    requested_output_in(&argv[1..], 0)
}

fn append_json_string(encoded: &mut String, value: &str) {
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character <= '\u{1f}' => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let value = character as usize;
                encoded.push_str("\\u00");
                encoded.push(HEX[value >> 4] as char);
                encoded.push(HEX[value & 0x0f] as char);
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
}

fn encode_invocation_record(
    argv: &[OsString],
    route: Route,
    working_directory: &Path,
    output: Option<&str>,
) -> String {
    let mut encoded = String::from("{\"schema\":1,\"status\":\"success\",\"process_id\":");
    encoded.push_str(&std::process::id().to_string());
    encoded.push_str(",\"working_directory\":");
    append_json_string(&mut encoded, &working_directory.to_string_lossy());
    encoded.push_str(",\"engine\":");
    append_json_string(&mut encoded, route.engine.name);
    encoded.push_str(",\"route_kind\":");
    append_json_string(
        &mut encoded,
        if route.engine.native {
            "native"
        } else {
            "bridge"
        },
    );
    encoded.push_str(",\"reason\":");
    append_json_string(&mut encoded, route.reason.label());
    encoded.push_str(",\"output\":");
    if let Some(output) = output {
        append_json_string(&mut encoded, output);
    } else {
        encoded.push_str("null");
    }
    encoded.push_str(",\"arguments\":[");
    for (index, argument) in argv.iter().skip(1).enumerate() {
        if index != 0 {
            encoded.push(',');
        }
        append_json_string(&mut encoded, &argument.to_string_lossy());
    }
    encoded.push_str("]}\n");
    encoded
}

/// Records one successfully completed linker invocation when [`RELD_INVOCATION_LOG_ENV`] is set.
///
/// The JSONL record includes enough information for an external test to match the exact output
/// artifact and selected engine. Failure to append is an error: silently losing the audit record
/// would let an acceptance test pass without proving that reld handled the link.
pub fn log_successful_invocation(argv: &[OsString], route: Route) -> Result<()> {
    let Some(path) = std::env::var_os(RELD_INVOCATION_LOG_ENV) else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    let working_directory = std::env::current_dir()
        .with_context(|| "Failed to determine the linker working directory".to_owned())?;
    let output = requested_output(argv)?;
    let encoded = encode_invocation_record(argv, route, &working_directory, output.as_deref());
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open invocation log `{}`", path.display()))?;
    log.write_all(encoded.as_bytes())
        .with_context(|| format!("Failed to append invocation log `{}`", path.display()))?;
    Ok(())
}

impl Route {
    /// Whether the selected engine must be invoked through the subprocess bridge.
    #[must_use]
    pub fn is_bridge(self) -> bool {
        !self.engine.native
    }

    /// Emits the observable routing decision for a native link.
    pub fn log_native(self) {
        debug_assert!(self.engine.native);
        if !route_logging_enabled() {
            return;
        }
        eprintln!(
            "reld: engine={} (native, reason={})",
            self.engine.name,
            self.reason.label()
        );
    }
}

/// Selects the bridge engine to use for `target`, honoring an explicit override name if given.
///
/// - `override_name` is `None`: returns the target's default engine (today's behavior).
/// - `override_name` is `Some(name)`: looks the engine up by name. An unknown name is a hard
///   error listing the valid engine names; a known engine whose format doesn't match `target` is
///   a hard error naming both the engine and the requested format.
fn select_engine(
    target: BridgeTarget,
    override_name: Option<&str>,
    requirements: &[Requirement],
) -> Result<&'static Engine> {
    if let Some(name) = override_name {
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

        if let Some(requirement) = requirements
            .iter()
            .find(|requirement| !engine.capabilities.contains(&requirement.capability))
        {
            bail!(
                "Engine `{name}` does not support {} requested by `{}`.",
                requirement.capability.label(),
                requirement.trigger,
            );
        }

        return Ok(engine);
    }

    let default = Engine::default_for(target);
    if default.supports(requirements) {
        return Ok(default);
    }

    if let Some(engine) = ENGINES
        .iter()
        .find(|engine| engine.format == target && engine.supports(requirements))
    {
        return Ok(engine);
    }

    let Some(requirement) = requirements
        .iter()
        .find(|requirement| !default.capabilities.contains(&requirement.capability))
    else {
        bail!("Internal error: capability fallback failed without an unmet requirement");
    };
    bail!(
        "No bundled {} engine supports {} requested by `{}`.",
        target.format_label(),
        requirement.capability.label(),
        requirement.trigger,
    );
}

fn collect_requested_capabilities(
    args: impl IntoIterator<Item = OsString>,
    requirements: &mut Vec<Requirement>,
    response_depth: usize,
) -> Result<()> {
    for arg in args {
        let Some(arg) = arg.to_str() else {
            continue;
        };
        if let Some(path) = arg.strip_prefix('@') {
            if response_depth >= 16 {
                bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
            }
            let nested = crate::args::read_args_from_file(Path::new(path))?
                .into_iter()
                .map(OsString::from);
            collect_requested_capabilities(nested, requirements, response_depth + 1)?;
            continue;
        }

        let lower = arg.to_ascii_lowercase();
        let requirement = if matches!(
            lower.as_str(),
            "--validate-output" | "--write-layout" | "--write-trace"
        ) {
            Some(Requirement {
                capability: Capability::NativeControl,
                trigger: "flag:reld-only-control",
            })
        } else if is_driver_lto_flag(&lower) {
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:-flto",
            })
        } else if lower == "-plugin"
            || lower == "--plugin"
            || lower.starts_with("-plugin=")
            || lower.starts_with("--plugin=")
        {
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:--plugin",
            })
        } else if lower == "/ltcg"
            || lower.starts_with("/ltcg:")
            || lower == "/gl"
            || lower.starts_with("/gl:")
        {
            Some(Requirement {
                capability: Capability::Lto,
                trigger: "flag:/LTCG",
            })
        } else if lower == "--icf=all"
            || lower == "--icf=safe"
            || lower == "-icf=all"
            || lower == "-icf=safe"
        {
            Some(Requirement {
                capability: Capability::Icf,
                trigger: "flag:--icf",
            })
        } else if lower == "--discard-all" || lower == "-discard-all" || lower == "-x" {
            Some(Requirement {
                capability: Capability::DiscardAll,
                trigger: "flag:--discard-all",
            })
        } else if lower == "--fatal-warnings" || lower == "-fatal-warnings" {
            Some(Requirement {
                capability: Capability::FatalWarnings,
                trigger: "flag:--fatal-warnings",
            })
        } else if lower == "--color-diagnostics"
            || lower.starts_with("--color-diagnostics=")
            || lower == "--no-color-diagnostics"
        {
            Some(Requirement {
                capability: Capability::ColorDiagnostics,
                trigger: "flag:--color-diagnostics",
            })
        } else if lower == "--no-undefined-version"
            || lower == "-no-undefined-version"
            || lower == "--undefined-version"
            || lower == "-undefined-version"
        {
            Some(Requirement {
                capability: Capability::VersionScriptPolicy,
                trigger: "flag:--no-undefined-version",
            })
        } else if lower == "--fix-cortex-a53-843419" || lower == "-fix-cortex-a53-843419" {
            Some(Requirement {
                capability: Capability::CortexA53Erratum,
                trigger: "flag:--fix-cortex-a53-843419",
            })
        } else {
            None
        };

        if let Some(requirement) = requirement
            && !requirements
                .iter()
                .any(|existing: &Requirement| existing.capability == requirement.capability)
        {
            requirements.push(requirement);
        }
    }

    Ok(())
}

fn requested_capabilities(argv: &[OsString]) -> Result<Vec<Requirement>> {
    let mut requirements = Vec::new();
    collect_requested_capabilities(argv.iter().skip(1).cloned(), &mut requirements, 0)?;
    Ok(requirements)
}

fn override_from_request(
    argv: &[OsString],
    env_override: Option<&str>,
) -> (Option<String>, SelectionReason) {
    match engine_override_from_argv(argv) {
        Some(name) => (Some(name), SelectionReason::OverrideFlag),
        None => match env_override {
            Some(name) => (Some(name.to_owned()), SelectionReason::OverrideEnv),
            None => (None, SelectionReason::Default),
        },
    }
}

fn select_route_with_policy(
    argv: &[OsString],
    target: BridgeTarget,
    env_override: Option<&str>,
    allow_unsupported_native: bool,
    native_control_env: bool,
) -> Result<Route> {
    // COFF and Mach-O each have a single bundled engine today, so parsing their response files
    // cannot affect selection. More importantly, their response grammars and encodings differ
    // from ELF/GNU (MSVC commonly emits UTF-16). Leave them byte-for-byte for the destination
    // driver instead of eagerly feeding them through reld's UTF-8 GNU response parser.
    let mut requirements = if target == BridgeTarget::Elf {
        requested_capabilities(argv)?
    } else {
        Vec::new()
    };
    if target == BridgeTarget::Elf
        && native_control_env
        && !requirements
            .iter()
            .any(|requirement| requirement.capability == Capability::NativeControl)
    {
        requirements.push(Requirement {
            capability: Capability::NativeControl,
            trigger: "environment:reld-only-control",
        });
    }
    let (override_name, override_reason) = override_from_request(argv, env_override);
    let checked_requirements =
        if allow_unsupported_native && override_name.as_deref() == Some(NATIVE_RELD_ENGINE.name) {
            &[][..]
        } else {
            requirements.as_slice()
        };
    let engine = select_engine(target, override_name.as_deref(), checked_requirements)?;
    let default = Engine::default_for(target);
    let reason = if override_name.is_some() {
        override_reason
    } else if engine != default {
        let Some(requirement) = requirements.first() else {
            bail!("Internal error: non-default route selected without a capability requirement");
        };
        SelectionReason::Capability(requirement.trigger)
    } else {
        SelectionReason::Default
    };

    Ok(Route { engine, reason })
}

#[cfg(test)]
fn select_route_with_env(
    argv: &[OsString],
    target: BridgeTarget,
    env_override: Option<&str>,
) -> Result<Route> {
    select_route_with_policy(argv, target, env_override, false, false)
}

/// Selects the engine for one raw linker invocation.
pub fn select_route(argv: &[OsString], target: BridgeTarget) -> Result<Route> {
    let native_control_env = [
        crate::args::VALIDATE_ENV,
        crate::args::WRITE_LAYOUT_ENV,
        crate::args::WRITE_TRACE_ENV,
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    let allow_unsupported_native = std::env::var(crate::args::RELD_UNSUPPORTED_ENV)
        .ok()
        .as_deref()
        == Some("ignore");

    select_route_with_policy(
        argv,
        target,
        std::env::var(RELD_ENGINE_ENV).ok().as_deref(),
        allow_unsupported_native,
        native_control_env,
    )
}

/// Locates the linker binary that the bridge should delegate to for the given engine.
///
/// Precedence:
/// 1. `RELD_BRIDGE_LINKER` env var, used verbatim. Errors if the path doesn't exist.
/// 2. `rust-lld` next to the active toolchain (`rustc --print sysroot`).
/// 3. `gcc-ld/<basename>` under that same rustlib bin dir, then `<basename>` on `PATH`.
/// 4. Otherwise, a hard error naming `RELD_BRIDGE_LINKER` and what to install.
fn discover_linker(engine: &Engine) -> Result<PathBuf> {
    debug_assert!(!engine.native);
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

/// Removes compiler-driver-only LTO switches after they have served their routing purpose. LLD
/// consumes LLVM bitcode directly and rejects `-flto` itself; plugin switches are real linker
/// options and remain untouched.
fn forwarded_args_for_engine<I: IntoIterator<Item = OsString>>(
    argv: I,
    engine: &Engine,
) -> Result<Vec<OsString>> {
    let forwarded = forwarded_args(argv);
    if engine.name == "lld" {
        return expand_and_strip_driver_lto(forwarded, 0);
    }
    Ok(forwarded)
}

fn is_driver_lto_flag(arg: &str) -> bool {
    arg.eq_ignore_ascii_case("-flto")
        || arg.eq_ignore_ascii_case("--flto")
        || arg.to_ascii_lowercase().starts_with("-flto=")
        || arg.to_ascii_lowercase().starts_with("--flto=")
}

fn expand_and_strip_driver_lto(
    args: impl IntoIterator<Item = OsString>,
    response_depth: usize,
) -> Result<Vec<OsString>> {
    let mut forwarded = Vec::new();
    for arg in args {
        if let Some(arg) = arg.to_str() {
            if is_driver_lto_flag(arg) {
                continue;
            }
            if let Some(path) = arg.strip_prefix('@') {
                if response_depth >= 16 {
                    bail!("Linker response-file nesting exceeds 16 levels at `{path}`");
                }
                let nested = crate::args::read_args_from_file(Path::new(path))?
                    .into_iter()
                    .map(OsString::from);
                forwarded.extend(expand_and_strip_driver_lto(nested, response_depth + 1)?);
                continue;
            }
        }
        forwarded.push(arg);
    }
    Ok(forwarded)
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
pub fn run_bridge<I: IntoIterator<Item = OsString>>(argv: I, route: Route) -> Result<()> {
    let argv: Vec<OsString> = argv.into_iter().collect();
    if !route.is_bridge() {
        bail!("Internal error: native engine passed to the subprocess bridge");
    }
    let engine = route.engine;
    let linker = discover_linker(engine)?;

    let forwarded = forwarded_args_for_engine(argv, engine)?;
    let child_args = child_command_line(&linker, engine, forwarded);

    if route_logging_enabled() {
        eprintln!(
            "reld: engine={} (bridge, reason={}) -> {}",
            engine.name,
            route.reason.label(),
            linker.display()
        );
    }

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
    // linker-control environment variables.
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
            Self::create_with_contents(name, b"")
        }

        fn create_with_contents(name: &str, contents: &[u8]) -> Self {
            let path = unique_temp_path(name);
            std::fs::write(&path, contents).unwrap();
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

        let discovered = discover_linker(Engine::find("lld").unwrap()).unwrap();
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
    fn successful_invocation_log_records_exact_output_and_route() {
        let _lock = ENV_LOCK.lock().unwrap();
        let log = TempFile::create("successful-invocation.jsonl");
        let _guard = EnvVarGuard::set(RELD_INVOCATION_LOG_ENV, log.path().to_str().unwrap());
        let argv = vec![
            OsString::from("reld-link"),
            OsString::from("input.obj"),
            OsString::from("/OUT:C:/build/consumer.exe"),
        ];
        let route = Route {
            engine: &COFF_LLD_ENGINE,
            reason: SelectionReason::Default,
        };

        log_successful_invocation(&argv, route).unwrap();

        let contents = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let record: serde_yaml::Value = serde_yaml::from_str(lines[0]).unwrap();
        assert_eq!(record["schema"], 1);
        assert_eq!(record["status"], "success");
        assert_eq!(record["engine"], "lld-link");
        assert_eq!(record["route_kind"], "bridge");
        assert_eq!(record["reason"], "default");
        assert_eq!(record["output"], "C:/build/consumer.exe");
        assert_eq!(record["arguments"][0], "input.obj");
        assert_eq!(record["arguments"][1], "/OUT:C:/build/consumer.exe");
        assert!(record["process_id"].as_u64().unwrap() > 0);
        assert!(!record["working_directory"].as_str().unwrap().is_empty());
    }

    #[test]
    fn requested_output_preserves_gnu_and_coff_path_spelling() {
        let gnu = vec![
            OsString::from("reld"),
            OsString::from("-o"),
            OsString::from("Build/Mixed Case/app"),
        ];
        assert_eq!(
            requested_output(&gnu).unwrap().as_deref(),
            Some("Build/Mixed Case/app")
        );

        let coff = vec![
            OsString::from("reld-link"),
            OsString::from("-OUT:C:/Build/Mixed Case/app.exe"),
        ];
        assert_eq!(
            requested_output(&coff).unwrap().as_deref(),
            Some("C:/Build/Mixed Case/app.exe")
        );

        let response = TempFile::create_with_contents(
            "coff-output-response",
            br#"input.obj "/OUT:C:\Build\Mixed Case\response.exe" /DEBUG"#,
        );
        let response_argv = vec![
            OsString::from("reld-link"),
            OsString::from(format!("@{}", response.path().display())),
        ];
        assert_eq!(
            requested_output(&response_argv).unwrap().as_deref(),
            Some(r"C:\Build\Mixed Case\response.exe")
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
        for engine in [
            Engine::find("lld").unwrap(),
            Engine::default_for(BridgeTarget::Coff),
            Engine::default_for(BridgeTarget::MachO),
        ] {
            let message = not_found_message(engine);
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
    fn elf_flavor_prefix_is_gnu() {
        let forwarded = vec![OsString::from("-o"), OsString::from("a.out")];
        let child = child_command_line(
            Path::new("/tc/rust-lld"),
            Engine::find("lld").unwrap(),
            forwarded,
        );
        assert_eq!(child[0], OsString::from("-flavor"));
        assert_eq!(child[1], OsString::from("gnu"));
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
        let engine = select_engine(BridgeTarget::Coff, None, &[]).unwrap();
        assert_eq!(engine.name, "lld-link");
        assert_eq!(engine.format, BridgeTarget::Coff);
    }

    #[test]
    fn select_engine_default_for_macho_is_ld64_lld() {
        let engine = select_engine(BridgeTarget::MachO, None, &[]).unwrap();
        assert_eq!(engine.name, "ld64.lld");
        assert_eq!(engine.format, BridgeTarget::MachO);
    }

    #[test]
    fn select_engine_valid_override_matches_target() {
        let engine = select_engine(BridgeTarget::Coff, Some("lld-link"), &[]).unwrap();
        assert_eq!(engine.name, "lld-link");

        let engine = select_engine(BridgeTarget::MachO, Some("ld64.lld"), &[]).unwrap();
        assert_eq!(engine.name, "ld64.lld");
    }

    #[test]
    fn select_engine_unknown_name_errors_listing_valid_engines() {
        let err = select_engine(BridgeTarget::Coff, Some("bogus"), &[]).unwrap_err();
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
        let err = select_engine(BridgeTarget::Coff, Some("ld64.lld"), &[]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("ld64.lld"),
            "unexpected message: {message}"
        );
        assert!(message.contains("COFF"), "unexpected message: {message}");

        // Symmetric direction: lld-link links COFF; requesting it for a Mach-O link mismatches.
        let err = select_engine(BridgeTarget::MachO, Some("lld-link"), &[]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("lld-link"),
            "unexpected message: {message}"
        );
        assert!(message.contains("Mach-O"), "unexpected message: {message}");
    }

    #[test]
    fn elf_defaults_to_native_reld() {
        let route = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("foo.o")],
            BridgeTarget::Elf,
            None,
        )
        .unwrap();
        assert_eq!(route.engine.name, "reld");
        assert!(!route.is_bridge());
        assert_eq!(route.reason, SelectionReason::Default);
    }

    #[test]
    fn explicit_native_override_can_accept_capabilities_with_ignore_policy() {
        let route = select_route_with_policy(
            &[
                OsString::from("ld.reld"),
                OsString::from("-flto"),
                OsString::from("foo.o"),
            ],
            BridgeTarget::Elf,
            Some("reld"),
            true,
            false,
        )
        .unwrap();
        assert_eq!(route.engine.name, "reld");
        assert!(!route.is_bridge());
        assert_eq!(route.reason, SelectionReason::OverrideEnv);
    }

    #[test]
    fn ignore_policy_does_not_suppress_conflicting_lld_override() {
        let error = select_route_with_policy(
            &[
                OsString::from("ld.reld"),
                OsString::from("--validate-output"),
            ],
            BridgeTarget::Elf,
            Some("lld"),
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("reld-only validation"));
    }

    #[test]
    fn ignore_policy_does_not_suppress_unknown_override() {
        let error = select_route_with_policy(
            &[OsString::from("ld.reld"), OsString::from("foo.o")],
            BridgeTarget::Elf,
            Some("bogus"),
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Unknown engine `bogus`"));
    }

    #[test]
    fn elf_lto_spellings_route_to_lld() {
        for flag in [
            "-flto",
            "-flto=thin",
            "--flto=full",
            "--plugin=/tool/LLVMgold.so",
            "-plugin",
        ] {
            let route = select_route_with_env(
                &[
                    OsString::from("ld.reld"),
                    OsString::from(flag),
                    OsString::from("foo.o"),
                ],
                BridgeTarget::Elf,
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
            assert!(route.is_bridge(), "flag {flag}");
            assert!(matches!(route.reason, SelectionReason::Capability(_)));
        }
    }

    #[test]
    fn elf_icf_routes_to_lld_but_disabled_icf_stays_native() {
        for flag in ["--icf=all", "--icf=safe"] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf,
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
        }

        let native = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("--icf=none")],
            BridgeTarget::Elf,
            None,
        )
        .unwrap();
        assert_eq!(native.engine.name, "reld");
    }

    #[test]
    fn unsupported_native_semantics_route_to_lld() {
        for flag in [
            "--discard-all",
            "-x",
            "--fatal-warnings",
            "--color-diagnostics=always",
            "--no-color-diagnostics",
            "--no-undefined-version",
            "--undefined-version",
            "--fix-cortex-a53-843419",
        ] {
            let route = select_route_with_env(
                &[OsString::from("ld.reld"), OsString::from(flag)],
                BridgeTarget::Elf,
                None,
            )
            .unwrap();
            assert_eq!(route.engine.name, "lld", "flag {flag}");
        }
    }

    #[test]
    fn linker_plugin_inside_response_file_routes_to_lld() {
        let response =
            TempFile::create_with_contents("lto-response", b"--plugin=/tool/LLVMgold.so foo.o");
        let route = select_route_with_env(
            &[
                OsString::from("ld.reld"),
                OsString::from(format!("@{}", response.path().display())),
            ],
            BridgeTarget::Elf,
            None,
        )
        .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::Capability("flag:--plugin"));
    }

    #[test]
    fn non_elf_response_files_are_forwarded_without_gnu_or_utf8_parsing() {
        // UTF-16LE response data with Windows quoting. This must remain opaque to the router;
        // lld-link/ld64.lld own their response-file grammar and encoding.
        let response = TempFile::create_with_contents(
            "windows-utf16-response",
            &[
                0xff, 0xfe, b'/', 0, b'O', 0, b'U', 0, b'T', 0, b':', 0, b'"', 0, b'a', 0, b' ', 0,
                b'b', 0, b'.', 0, b'e', 0, b'x', 0, b'e', 0, b'"', 0,
            ],
        );
        let response_arg = OsString::from(format!("@{}", response.path().display()));

        for target in [BridgeTarget::Coff, BridgeTarget::MachO] {
            let argv = [OsString::from("reld"), response_arg.clone()];
            let route = select_route_with_env(&argv, target, None).unwrap();
            assert_eq!(route.engine, Engine::default_for(target));
            assert_eq!(
                forwarded_args_for_engine(argv, route.engine).unwrap(),
                vec![response_arg.clone()]
            );
        }
    }

    #[test]
    fn elf_lld_strips_driver_lto_flag_but_keeps_plugin_flag() {
        let argv = [
            OsString::from("ld.reld"),
            OsString::from("--engine=lld"),
            OsString::from("-flto=thin"),
            OsString::from("--plugin=/tool/LLVMgold.so"),
            OsString::from("foo.o"),
        ];
        assert_eq!(
            forwarded_args_for_engine(argv, Engine::find("lld").unwrap()).unwrap(),
            vec![
                OsString::from("--plugin=/tool/LLVMgold.so"),
                OsString::from("foo.o"),
            ]
        );
    }

    #[test]
    fn elf_lld_expands_response_files_and_strips_nested_driver_lto_flag() {
        let nested = TempFile::create_with_contents(
            "nested-lto-response",
            b"-flto=thin --build-id=sha1 nested.o",
        );
        let nested_path = nested.path().to_string_lossy().replace('\\', "/");
        let outer = TempFile::create_with_contents(
            "outer-lto-response",
            format!("@{nested_path} outer.o").as_bytes(),
        );
        let argv = [
            OsString::from("ld.reld"),
            OsString::from(format!("@{}", outer.path().display())),
        ];

        let route = select_route_with_env(&argv, BridgeTarget::Elf, None).unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(
            forwarded_args_for_engine(argv, route.engine).unwrap(),
            vec![
                OsString::from("--build-id=sha1"),
                OsString::from("nested.o"),
                OsString::from("outer.o"),
            ]
        );
    }

    #[test]
    fn explicit_engine_flag_takes_precedence_over_environment() {
        let route = select_route_with_env(
            &[OsString::from("ld.reld"), OsString::from("--engine=lld")],
            BridgeTarget::Elf,
            Some("reld"),
        )
        .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::OverrideFlag);
    }

    #[test]
    fn environment_can_force_elf_lld() {
        let route =
            select_route_with_env(&[OsString::from("ld.reld")], BridgeTarget::Elf, Some("lld"))
                .unwrap();
        assert_eq!(route.engine.name, "lld");
        assert_eq!(route.reason, SelectionReason::OverrideEnv);
    }

    #[test]
    fn forcing_native_reld_for_lto_is_a_clear_error() {
        let error = select_route_with_env(
            &[
                OsString::from("ld.reld"),
                OsString::from("--engine=reld"),
                OsString::from("-flto"),
            ],
            BridgeTarget::Elf,
            None,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("reld"), "unexpected message: {message}");
        assert!(message.contains("LTO"), "unexpected message: {message}");
        assert!(message.contains("-flto"), "unexpected message: {message}");
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
