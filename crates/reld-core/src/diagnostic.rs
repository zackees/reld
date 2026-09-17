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
}
