//! The declarative flag table (reld#123 Phase 1 / D1).
//!
//! A single four-way classification over every flag reld and its bundled engines know about,
//! replacing the scattered `SATISFIED_BY_CONSTRUCTION_FLAGS`, `IGNORED_FLAGS`, `DEFAULT_FLAGS`,
//! `DEFAULT_SHORT_FLAGS`, the `-z` fallback, and the `if lower == …` chain in
//! `collect_requested_capabilities`.
//!
//! This module currently holds the *initial* table covering the flags reld already classifies;
//! Phase 1 vendors lld's `Options.td` and GNU ld's option list to grow it to the full union, and
//! a CI check will fail any inventory entry lacking a rule.

use crate::bridge::Capability;

/// How a flag is honored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// The native parser implements it; the rule names the test that proves it.
    Native,
    /// The native engine's unconditional behavior already implies it; the rule names the
    /// equivalence test (native vs lld) comparing the property the flag governs.
    SatisfiedByConstruction,
    /// Native cannot honor it; route to the first engine whose capability set includes it.
    Requires(Capability),
    /// No bundled engine honors it; loud error unless `RELD_UNSUPPORTED=ignore`.
    Unsupported,
}

/// Which tools emit this flag, for the Phase-4 corpus (D4). Cosmetic in the table; the corpus
/// cross-checks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Variants are constructed as the table grows (full D1 migration).
pub(crate) enum Emitter {
    Rustc,
    Clang,
    Gcc,
    CcRs,
    Cmake,
    HandWritten,
}

/// How the flag's value is attached, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Separate` is used by the full D1 migration, not the initial table.
pub(crate) enum ValueMatch {
    /// Boolean flag: no value.
    None,
    /// `--flag=value` (or `-zvalue`, `/FLAG:value`).
    Joined,
    /// `--flag value` (value as the next token).
    Separate,
    /// A `-z` keyword sub-option.
    ZKeyword,
}

/// One flag (or set of equivalent spellings) and its disposition.
#[derive(Debug)]
pub(crate) struct FlagRule {
    /// Exact spellings, case-sensitive for GNU/ld64, case-insensitive for COFF.
    pub spellings: &'static [&'static str],
    /// How the value is attached. Consumed by the full D1 migration, not the bootstrap renderer.
    #[allow(dead_code)]
    pub value: ValueMatch,
    /// How the flag is honored.
    pub disposition: Disposition,
    /// Which emitters produce it (informational; cross-checked by the D4 corpus).
    pub emitters: &'static [Emitter],
}

const fn rule(
    spellings: &'static [&'static str],
    value: ValueMatch,
    disposition: Disposition,
    emitters: &'static [Emitter],
) -> FlagRule {
    FlagRule {
        spellings,
        value,
        disposition,
        emitters,
    }
}

/// The initial flag table. Phase 1 grows this to the full lld/GNU union.
pub(crate) static FLAG_TABLE: &[FlagRule] = &[
    // --- Satisfied-by-construction (reld#123 §A/§B) ---
    rule(
        &["--start-group", "--end-group"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &["-("],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &[")"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &["--nostdlib"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &["--sort-common"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::HandWritten],
    ),
    rule(
        &["--stats"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::HandWritten],
    ),
    rule(
        &["--no-undefined-version", "--undefined-version"],
        ValueMatch::None,
        Disposition::SatisfiedByConstruction,
        &[Emitter::Rustc],
    ),
    // --- Routed capabilities (native cannot honor them) ---
    rule(
        &[
            "--validate-output",
            "--write-layout",
            "--write-trace",
            "--sym-info",
        ],
        ValueMatch::None,
        Disposition::Requires(Capability::NativeControl),
        &[Emitter::HandWritten],
    ),
    rule(
        &["--fatal-warnings"],
        ValueMatch::None,
        Disposition::Requires(Capability::FatalWarnings),
        &[Emitter::Clang],
    ),
    rule(
        &["--color-diagnostics", "--no-color-diagnostics"],
        ValueMatch::None,
        Disposition::Requires(Capability::ColorDiagnostics),
        &[Emitter::Clang],
    ),
    rule(
        &["--icf=all", "--icf=safe"],
        ValueMatch::Joined,
        Disposition::Requires(Capability::Icf),
        &[Emitter::HandWritten],
    ),
    rule(
        &["--discard-all", "-x"],
        ValueMatch::None,
        Disposition::Requires(Capability::DiscardAll),
        &[Emitter::HandWritten],
    ),
    rule(
        &["--fix-cortex-a53-843419"],
        ValueMatch::None,
        Disposition::Requires(Capability::CortexA53Erratum),
        &[Emitter::HandWritten],
    ),
    rule(
        &["-z text"],
        ValueMatch::ZKeyword,
        Disposition::Requires(Capability::TextRelocs),
        &[Emitter::HandWritten],
    ),
    rule(
        &["-flto", "--flto"],
        ValueMatch::None,
        Disposition::Requires(Capability::Lto),
        &[Emitter::Clang, Emitter::Gcc, Emitter::Rustc],
    ),
    // --- Native defaults (native maps them to its default behavior) ---
    rule(
        &["--no-call-graph-profile-sort"],
        ValueMatch::None,
        Disposition::Native,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &["--no-copy-dt-needed-entries", "--no-add-needed"],
        ValueMatch::None,
        Disposition::Native,
        &[Emitter::Clang, Emitter::Gcc],
    ),
    rule(
        &["--discard-locals", "-X"],
        ValueMatch::None,
        Disposition::Native,
        &[Emitter::HandWritten],
    ),
    rule(
        &["--no-fatal-warnings"],
        ValueMatch::None,
        Disposition::Native,
        &[Emitter::Clang],
    ),
    // --- Unsupported (no bundled engine honors it) ---
    rule(
        &["--fix-cortex-a53-835769"],
        ValueMatch::None,
        Disposition::Unsupported,
        &[Emitter::HandWritten],
    ),
];

/// Renders the flag table as rows of `spelling | disposition | emitters`.
pub(crate) fn render_flag_table() -> String {
    let mut out = String::new();
    out.push_str("spelling | disposition | emitters\n");
    out.push_str("--- | --- | ---\n");
    for flag in FLAG_TABLE {
        let spellings = flag.spellings.join(", ");
        let disposition = match flag.disposition {
            Disposition::Native => "Native".to_owned(),
            Disposition::SatisfiedByConstruction => "SatisfiedByConstruction".to_owned(),
            Disposition::Requires(capability) => format!("Requires({})", capability.label()),
            Disposition::Unsupported => "Unsupported".to_owned(),
        };
        let emitters = flag
            .emitters
            .iter()
            .map(|emitter| match emitter {
                Emitter::Rustc => "rustc",
                Emitter::Clang => "clang",
                Emitter::Gcc => "gcc",
                Emitter::CcRs => "cc-rs",
                Emitter::Cmake => "cmake",
                Emitter::HandWritten => "hand-written",
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("{spellings} | {disposition} | {emitters}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_table_has_no_duplicate_spellings() {
        let mut seen = std::collections::HashSet::new();
        for flag in FLAG_TABLE {
            for spelling in flag.spellings {
                assert!(
                    seen.insert(*spelling),
                    "duplicate spelling `{spelling}` in the flag table"
                );
            }
        }
    }

    #[test]
    fn flag_table_renders_every_row() {
        let rendered = render_flag_table();
        for flag in FLAG_TABLE {
            for spelling in flag.spellings {
                assert!(
                    rendered.contains(spelling),
                    "rendered table missing `{spelling}`"
                );
            }
        }
    }
}
