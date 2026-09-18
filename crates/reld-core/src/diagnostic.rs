//! Diagnostic rendering for the native engine.
//!
//! Every `reld: …` diagnostic is produced here so one decision governs whether they are colored.
//! Host terminal mechanics live behind [`crate::platforms::term`]; this module only decides and
//! formats.

use crate::platforms::term::StderrTerminal;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

/// Whether diagnostics are colored, as requested on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Always color, whatever stderr is.
    Always,
    /// Never color.
    Never,
    /// Color only when stderr is a terminal that renders ANSI.
    #[default]
    Auto,
}

/// How severe a diagnostic is. Chooses the label and its color, following lld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    const fn label(self) -> &'static str {
        match self {
            Severity::Error => "error: ",
            Severity::Warning => "warning: ",
            Severity::Note => "note: ",
        }
    }

    /// The SGR parameters lld uses: red for errors, magenta for warnings, bold for notes.
    const fn ansi_parameters(self) -> &'static str {
        match self {
            Severity::Error => "0;31",
            Severity::Warning => "0;35",
            Severity::Note => "0;1",
        }
    }
}

static CHOICE: AtomicU8 = AtomicU8::new(CHOICE_UNSET);
const CHOICE_UNSET: u8 = 0;
const CHOICE_ALWAYS: u8 = 1;
const CHOICE_NEVER: u8 = 2;
const CHOICE_AUTO: u8 = 3;

/// Probed at most once: on Windows this switches the console into virtual-terminal mode, which
/// should happen a single time per process.
fn stderr_terminal() -> StderrTerminal {
    static TERMINAL: OnceLock<StderrTerminal> = OnceLock::new();
    *TERMINAL.get_or_init(crate::platforms::term::prepare_stderr_for_ansi)
}

/// Records the requested color mode. The last call wins, matching `ld.lld`.
pub fn set_color_choice(choice: ColorChoice) {
    let encoded = match choice {
        ColorChoice::Always => CHOICE_ALWAYS,
        ColorChoice::Never => CHOICE_NEVER,
        ColorChoice::Auto => CHOICE_AUTO,
    };
    CHOICE.store(encoded, Ordering::Relaxed);
    // Keep `colored` (used by the symbol-info printer) on the same decision.
    colored::control::set_override(color_enabled());
}

/// Parses a `--color-diagnostics[=<value>]` argument value. `None` is returned for a value this
/// flag does not accept, so the caller can report it.
#[must_use]
pub fn color_choice_from_value(value: Option<&str>) -> Option<ColorChoice> {
    match value {
        None | Some("always") => Some(ColorChoice::Always),
        Some("never") => Some(ColorChoice::Never),
        Some("auto") => Some(ColorChoice::Auto),
        Some(_) => None,
    }
}

/// The color flag `argv` asks for, if any. Long flags are also accepted with a single dash, as
/// everywhere else in reld, and the last one wins. An invalid value is ignored here and left for
/// the parser to report.
fn color_choice_from_argv<S: AsRef<str>, I: Iterator<Item = S>>(argv: I) -> Option<ColorChoice> {
    let mut choice = None;
    for argument in argv {
        let argument = argument.as_ref();
        let Some(flag) = argument
            .strip_prefix("--")
            .or_else(|| argument.strip_prefix('-'))
        else {
            continue;
        };
        if flag == "no-color-diagnostics" {
            choice = Some(ColorChoice::Never);
        } else if let Some(value) = flag.strip_prefix("color-diagnostics") {
            let value = match value {
                "" => None,
                rest => Some(rest.strip_prefix('=').unwrap_or(rest)),
            };
            if let Some(parsed) = color_choice_from_value(value) {
                choice = Some(parsed);
            }
        }
    }
    choice
}

/// Applies any color flag present in `argv`, so diagnostics raised while parsing the rest of the
/// command line already honor it.
pub fn apply_color_flags_from_argv<S: AsRef<str>, I: Iterator<Item = S>>(argv: I) {
    if let Some(choice) = color_choice_from_argv(argv) {
        set_color_choice(choice);
    }
}

/// Whether diagnostics are currently colored.
#[must_use]
pub fn color_enabled() -> bool {
    let choice = match CHOICE.load(Ordering::Relaxed) {
        CHOICE_ALWAYS => ColorChoice::Always,
        CHOICE_NEVER => return false,
        _ => ColorChoice::Auto,
    };
    // Probe (and on Windows prepare) the terminal even when `always` forces the answer, so forced
    // color still renders in a console that needs virtual-terminal mode switched on.
    let terminal = stderr_terminal();
    should_color(choice, &ColorEnv::from_env(), terminal)
}

/// The environment variables that influence automatic color detection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ColorEnv {
    clicolor_force: Option<String>,
    no_color: Option<String>,
    term: Option<String>,
}

impl ColorEnv {
    fn from_env() -> Self {
        Self {
            clicolor_force: std::env::var("CLICOLOR_FORCE").ok(),
            no_color: std::env::var("NO_COLOR").ok(),
            term: std::env::var("TERM").ok(),
        }
    }
}

/// The color decision. Pure, so every host can test the whole table.
fn should_color(choice: ColorChoice, env: &ColorEnv, terminal: StderrTerminal) -> bool {
    match choice {
        ColorChoice::Always => return true,
        ColorChoice::Never => return false,
        ColorChoice::Auto => {}
    }

    if env
        .clicolor_force
        .as_deref()
        .is_some_and(|value| !value.is_empty() && value != "0")
    {
        return true;
    }
    if env
        .no_color
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return false;
    }
    if env.term.as_deref() == Some("dumb") {
        return false;
    }
    terminal == StderrTerminal::Ansi
}

/// Formats one diagnostic line: a plain `reld: ` prefix, then the severity label, then the
/// message. The label carries the color, including its colon and trailing space, as lld does.
fn format_diagnostic(severity: Severity, message: &dyn Display, colored: bool) -> String {
    let label = severity.label();
    if colored {
        format!(
            "reld: \x1b[{}m{label}\x1b[0m{message}",
            severity.ansi_parameters()
        )
    } else {
        format!("reld: {label}{message}")
    }
}

/// Renders a diagnostic using the current color decision.
#[must_use]
pub fn render(severity: Severity, message: &dyn Display) -> String {
    format_diagnostic(severity, message, color_enabled())
}

/// Writes a diagnostic to stderr.
pub fn emit(severity: Severity, message: &dyn Display) {
    eprintln!("{}", render(severity, message));
}

/// One reference to an undefined symbol, in the pieces `ld.lld` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UndefinedReference {
    /// `file:line` from debug info, already rendered by [`source_location`]. `None` without DWARF.
    pub(crate) source: Option<String>,
    /// The referencing object as lld's `getObjMsg` names it: the path given for a plain object,
    /// or the member name for an archive member.
    pub(crate) object: String,
    /// The enclosing defined symbol (display name) or `<section>+0x<offset>`.
    pub(crate) location: String,
    /// The archive path when `object` is an archive member.
    pub(crate) archive: Option<String>,
}

/// lld prints at most this many references per undefined symbol (`maxUndefReferences`).
pub(crate) const MAX_UNDEFINED_REFERENCES: usize = 3;

/// Formats a `file:line` location the way lld's `createFileLineMsg` does: the file name alone
/// when it already equals the full path, otherwise the file name followed by the full path in
/// parentheses.
pub(crate) fn source_location(path: &std::path::Path, line: u64) -> String {
    let full = path.display().to_string();
    let file = path
        .file_name()
        .map_or_else(|| full.clone(), |name| name.to_string_lossy().into_owned());
    if file == full {
        format!("{file}:{line}")
    } else {
        format!("{file}:{line} ({full}:{line})")
    }
}

/// Formats a location with no debug info as `<section>+0x<offset>`, matching lld's fallback.
pub(crate) fn section_offset_location(section: &str, offset: u64) -> String {
    format!("{section}+0x{offset:x}")
}

/// Builds the body of an `undefined symbol` diagnostic, mirroring lld's `reportUndefinedSymbol`:
/// the symbol name and visibility, then up to [`MAX_UNDEFINED_REFERENCES`] references, then a
/// count of any references left unshown.
pub(crate) fn undefined_symbol_message(
    visibility_prefix: &str,
    name: &dyn Display,
    references: &[UndefinedReference],
    total_references: usize,
) -> String {
    let mut message = format!("undefined {visibility_prefix}symbol: {name}");
    let shown = references.len().min(MAX_UNDEFINED_REFERENCES);
    for reference in &references[..shown] {
        message.push_str("\n>>> referenced by ");
        if let Some(source) = &reference.source {
            message.push_str(source);
            // Width of "referenced by ", so the object:(location) that follows lines up under it.
            message.push_str("\n>>>");
            message.push_str(&" ".repeat(15));
        }
        message.push_str(&format!("{}:({})", reference.object, reference.location));
        if let Some(archive) = &reference.archive {
            message.push_str(&format!(" in archive {archive}"));
        }
    }
    if total_references > shown {
        message.push_str(&format!(
            "\n>>> referenced {} more times",
            total_references - shown
        ));
    }
    message
}

/// Builds the body of the `--no-allow-shlib-undefined` diagnostic, mirroring lld's Writer.cpp
/// wording.
pub(crate) fn shlib_undefined_message(name: &dyn Display, shared_object: &dyn Display) -> String {
    format!(
        "undefined reference: {name}\n>>> referenced by {shared_object} \
         (disallowed by --no-allow-shlib-undefined)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(clicolor_force: Option<&str>, no_color: Option<&str>, term: Option<&str>) -> ColorEnv {
        ColorEnv {
            clicolor_force: clicolor_force.map(str::to_owned),
            no_color: no_color.map(str::to_owned),
            term: term.map(str::to_owned),
        }
    }

    #[test]
    fn explicit_choice_beats_environment_and_terminal() {
        let forced_off = env(Some("1"), Some("1"), Some("dumb"));
        assert!(should_color(
            ColorChoice::Always,
            &forced_off,
            StderrTerminal::NotATerminal
        ));
        assert!(!should_color(
            ColorChoice::Never,
            &env(Some("1"), None, Some("xterm")),
            StderrTerminal::Ansi
        ));
    }

    #[test]
    fn auto_follows_stderr_when_the_environment_is_quiet() {
        let quiet = env(None, None, Some("xterm-256color"));
        assert!(should_color(
            ColorChoice::Auto,
            &quiet,
            StderrTerminal::Ansi
        ));
        assert!(!should_color(
            ColorChoice::Auto,
            &quiet,
            StderrTerminal::NotATerminal
        ));
    }

    #[test]
    fn auto_honors_clicolor_force_no_color_and_dumb_terminals() {
        // CLICOLOR_FORCE wins over NO_COLOR, matching the `colored` semantics reld had before.
        assert!(should_color(
            ColorChoice::Auto,
            &env(Some("1"), Some("1"), None),
            StderrTerminal::NotATerminal
        ));
        assert!(!should_color(
            ColorChoice::Auto,
            &env(Some("0"), None, None),
            StderrTerminal::NotATerminal
        ));
        assert!(!should_color(
            ColorChoice::Auto,
            &env(None, Some("1"), None),
            StderrTerminal::Ansi
        ));
        // An empty NO_COLOR is not set, per the NO_COLOR specification.
        assert!(should_color(
            ColorChoice::Auto,
            &env(None, Some(""), None),
            StderrTerminal::Ansi
        ));
        assert!(!should_color(
            ColorChoice::Auto,
            &env(None, None, Some("dumb")),
            StderrTerminal::Ansi
        ));
    }

    #[test]
    fn colored_diagnostics_match_the_lld_layout() {
        assert_eq!(
            format_diagnostic(Severity::Error, &"boom", true),
            "reld: \x1b[0;31merror: \x1b[0mboom"
        );
        assert_eq!(
            format_diagnostic(Severity::Warning, &"careful", true),
            "reld: \x1b[0;35mwarning: \x1b[0mcareful"
        );
        assert_eq!(
            format_diagnostic(Severity::Note, &"detail", true),
            "reld: \x1b[0;1mnote: \x1b[0mdetail"
        );
    }

    #[test]
    fn plain_diagnostics_carry_no_escape_bytes() {
        for severity in [Severity::Error, Severity::Warning, Severity::Note] {
            let rendered = format_diagnostic(severity, &"message", false);
            assert!(!rendered.contains('\x1b'), "{rendered}");
        }
        assert_eq!(
            format_diagnostic(Severity::Error, &"boom", false),
            "reld: error: boom"
        );
    }

    #[test]
    fn flag_values_are_parsed_and_invalid_values_rejected() {
        assert_eq!(color_choice_from_value(None), Some(ColorChoice::Always));
        assert_eq!(
            color_choice_from_value(Some("always")),
            Some(ColorChoice::Always)
        );
        assert_eq!(
            color_choice_from_value(Some("never")),
            Some(ColorChoice::Never)
        );
        assert_eq!(
            color_choice_from_value(Some("auto")),
            Some(ColorChoice::Auto)
        );
        assert_eq!(color_choice_from_value(Some("sometimes")), None);
    }

    fn scanned_choice(argv: &[&str]) -> Option<ColorChoice> {
        color_choice_from_argv(argv.iter())
    }

    #[test]
    fn argv_scan_accepts_both_dash_forms_and_takes_the_last_flag() {
        assert_eq!(scanned_choice(&["ld.reld", "a.o"]), None);
        assert_eq!(
            scanned_choice(&["ld.reld", "--color-diagnostics"]),
            Some(ColorChoice::Always)
        );
        assert_eq!(
            scanned_choice(&["ld.reld", "-color-diagnostics=never"]),
            Some(ColorChoice::Never)
        );
        assert_eq!(
            scanned_choice(&["ld.reld", "--color-diagnostics=auto"]),
            Some(ColorChoice::Auto)
        );
        assert_eq!(
            scanned_choice(&["ld.reld", "--color-diagnostics", "--no-color-diagnostics"]),
            Some(ColorChoice::Never)
        );
        assert_eq!(
            scanned_choice(&[
                "ld.reld",
                "--no-color-diagnostics",
                "--color-diagnostics=always"
            ]),
            Some(ColorChoice::Always)
        );
        // An unknown value leaves the previous choice alone; the parser reports it.
        assert_eq!(
            scanned_choice(&[
                "ld.reld",
                "--color-diagnostics=never",
                "--color-diagnostics=nope"
            ]),
            Some(ColorChoice::Never)
        );
    }

    fn reference(source: Option<&str>, object: &str, location: &str) -> UndefinedReference {
        UndefinedReference {
            source: source.map(str::to_owned),
            object: object.to_owned(),
            location: location.to_owned(),
            archive: None,
        }
    }

    #[test]
    fn undefined_symbol_with_source_and_function_matches_lld() {
        let references = [reference(Some("a.c:3"), "a.o", "main")];
        let expected = format!(
            "undefined symbol: foo(int)\n>>> referenced by a.c:3\n>>>{}a.o:(main)",
            " ".repeat(15)
        );
        assert_eq!(
            undefined_symbol_message("", &"foo(int)", &references, 1),
            expected
        );
    }

    #[test]
    fn undefined_symbol_without_debug_info_names_object_and_function() {
        let references = [reference(None, "a.o", "main")];
        assert_eq!(
            undefined_symbol_message("", &"foo", &references, 1),
            "undefined symbol: foo\n>>> referenced by a.o:(main)"
        );
    }

    #[test]
    fn archive_members_and_section_offsets() {
        let references = [UndefinedReference {
            source: None,
            object: "b.o".to_owned(),
            location: section_offset_location(".text", 0x1c),
            archive: Some("libx.a".to_owned()),
        }];
        assert_eq!(
            undefined_symbol_message("", &"foo", &references, 1),
            "undefined symbol: foo\n>>> referenced by b.o:(.text+0x1c) in archive libx.a"
        );
    }

    #[test]
    fn hidden_visibility_is_named() {
        let message = undefined_symbol_message("hidden ", &"foo", &[], 0);
        assert!(message.starts_with("undefined hidden symbol: foo"));
    }

    #[test]
    fn references_beyond_three_are_counted() {
        let references = [
            reference(None, "r0.o", "main"),
            reference(None, "r1.o", "main"),
            reference(None, "r2.o", "main"),
            reference(None, "r3.o", "main"),
            reference(None, "r4.o", "main"),
        ];
        assert_eq!(
            undefined_symbol_message("", &"foo", &references, 5),
            "undefined symbol: foo\n\
             >>> referenced by r0.o:(main)\n\
             >>> referenced by r1.o:(main)\n\
             >>> referenced by r2.o:(main)\n\
             >>> referenced 2 more times"
        );
    }

    #[test]
    fn source_location_matches_create_file_line_msg() {
        assert_eq!(
            source_location(std::path::Path::new("/tmp/x/a.c"), 3),
            "a.c:3 (/tmp/x/a.c:3)"
        );
        assert_eq!(source_location(std::path::Path::new("a.c"), 3), "a.c:3");
    }

    #[test]
    fn shlib_undefined_matches_lld() {
        assert_eq!(
            shlib_undefined_message(&"foo", &"libbar.so"),
            "undefined reference: foo\n>>> referenced by libbar.so \
             (disallowed by --no-allow-shlib-undefined)"
        );
    }

    #[test]
    fn undefined_messages_carry_no_escape_bytes() {
        let references = [reference(Some("a.c:3"), "a.o", "main")];
        let message = undefined_symbol_message("", &"foo(int)", &references, 1);
        assert!(!message.contains('\x1b'), "{message}");
        assert_eq!(
            format_diagnostic(Severity::Error, &message, false),
            format!("reld: error: {message}")
        );
    }
}
