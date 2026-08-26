use reld_core::ensure;
use reld_core::error::Context;
use reld_core::error::Result;
use std::collections::HashSet;

pub(crate) fn should_enforce_aggregate_coverage(args: &libtest_mimic::Arguments) -> bool {
    args.filter.is_none() && args.skip.is_empty() && !args.list && !args.ignored && !args.bench
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackedIgnore {
    pattern: String,
    tracking_issue: u64,
    #[serde(default)]
    architectures: Vec<String>,
}

impl TrackedIgnore {
    pub(crate) fn parse_directive(value: &str) -> Result<Self> {
        let (pattern, issue) = value.split_once(" #").context(
            "DiffIgnore requires a tracking issue on the same line, for example `pattern #13`",
        )?;
        ensure!(!pattern.is_empty(), "DiffIgnore pattern must not be empty");
        let mut issue_parts = issue.split_whitespace();
        let tracking_issue = issue_parts
            .next()
            .context("DiffIgnore tracking issue is missing")?
            .parse::<u64>()
            .context("DiffIgnore tracking issue must be a positive integer prefixed by `#`")?;
        ensure!(
            tracking_issue > 0,
            "DiffIgnore tracking issue must be greater than zero"
        );
        let mut architectures = Vec::new();
        if let Some(scope) = issue_parts.next() {
            let values = scope
                .strip_prefix("arch=")
                .context("DiffIgnore scope must use `arch=name[,name...]`")?;
            architectures.extend(values.split(',').map(str::to_owned));
        }
        ensure!(
            issue_parts.next().is_none(),
            "DiffIgnore has unexpected trailing fields"
        );
        let tracked = Self {
            pattern: pattern.to_owned(),
            tracking_issue,
            architectures,
        };
        tracked.validate()?;
        Ok(tracked)
    }

    fn validate(&self) -> Result {
        ensure!(
            !self.pattern.is_empty(),
            "DiffIgnore pattern must not be empty"
        );
        ensure!(
            self.tracking_issue > 0,
            "DiffIgnore tracking issue must be greater than zero"
        );
        for arch in &self.architectures {
            ensure!(
                matches!(
                    arch.as_str(),
                    "x86_64" | "aarch64" | "riscv64" | "loongarch64" | "ppc64le"
                ),
                "DiffIgnore has unknown architecture `{arch}`"
            );
        }
        Ok(())
    }
}

pub(crate) fn ignore_patterns_for_arch(
    ignores: &[TrackedIgnore],
    arch: &str,
) -> Result<Vec<String>> {
    ignores
        .iter()
        .filter_map(|ignore| {
            if let Err(error) = ignore.validate() {
                return Some(Err(error));
            }
            let in_explicit_scope = ignore.architectures.is_empty()
                || ignore.architectures.iter().any(|item| item == arch);
            (in_explicit_scope && ignore_applies_to_arch(&ignore.pattern, arch))
                .then(|| Ok(ignore.pattern.clone()))
        })
        .collect()
}

pub(crate) fn validate_test_config_ignores(
    run_all_diffs: bool,
    ignores: &[TrackedIgnore],
) -> Result {
    for ignore in ignores {
        ignore.validate()?;
    }
    ensure!(
        !run_all_diffs || ignores.is_empty(),
        "run_all_diffs configs cannot supply diff_ignore entries because fixture-level ratcheting cannot prove a suite-global ignore is still needed"
    );
    Ok(())
}

fn ignore_applies_to_arch(pattern: &str, arch: &str) -> bool {
    let architecture_markers = [
        ("x86_64", ["R_X86_64_", "X86_64", "x86_64"]),
        ("aarch64", ["R_AARCH64_", "AARCH64", "aarch64"]),
        ("riscv64", ["R_RISCV_", "RISCV", "riscv"]),
        ("loongarch64", ["R_LARCH_", "LARCH", "loongarch"]),
        ("ppc64le", ["R_PPC64_", "PPC64", "ppc64"]),
    ];
    let mut mentioned_architecture = false;
    for (candidate, markers) in architecture_markers {
        if markers.iter().any(|marker| pattern.contains(marker)) {
            mentioned_architecture = true;
            if candidate == arch {
                return true;
            }
        }
    }
    if pattern.contains("__global_pointer$") {
        return arch == "riscv64";
    }
    if pattern.contains("section.ARM.attributes") {
        return false;
    }
    !mentioned_architecture
}

#[derive(Default)]
pub(crate) struct FixtureOracleValidation {
    report_count: usize,
    used_ignores: HashSet<String>,
}

impl FixtureOracleValidation {
    pub(crate) fn record_used_ignores(&mut self, used_ignores: impl IntoIterator<Item = String>) {
        self.report_count += 1;
        self.used_ignores.extend(used_ignores);
    }

    pub(crate) fn verify(&self, configured_ignores: &[String]) -> Result {
        if self.report_count == 0 {
            return Ok(());
        }
        let stale = configured_ignores
            .iter()
            .filter(|pattern| !self.used_ignores.contains(*pattern))
            .map(|pattern| {
                format!("ignore `{pattern}` is no longer needed; remove it from the fixture")
            })
            .collect::<Vec<_>>();
        ensure!(stale.is_empty(), "{}", stale.join("\n"));
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum OracleFormat {
    Elf,
    MachO,
}

#[derive(Default)]
struct FormatCoverage {
    report_count: usize,
    diffed_relocations: u64,
    total_relocations: u64,
}

impl FormatCoverage {
    fn percentage(&self) -> u64 {
        self.diffed_relocations
            .saturating_mul(100)
            .checked_div(self.total_relocations)
            .unwrap_or(0)
    }
}

#[derive(Default)]
pub(crate) struct AggregateOracleCoverage {
    elf: FormatCoverage,
    macho: FormatCoverage,
}

impl AggregateOracleCoverage {
    pub(crate) fn record(
        &mut self,
        format: OracleFormat,
        diffed_relocations: u64,
        total_relocations: u64,
    ) {
        let coverage = match format {
            OracleFormat::Elf => &mut self.elf,
            OracleFormat::MachO => &mut self.macho,
        };
        coverage.report_count += 1;
        coverage.diffed_relocations += diffed_relocations;
        coverage.total_relocations += total_relocations;
    }

    pub(crate) fn summaries(&self, floor: u64) -> Vec<String> {
        self.formats()
            .filter(|(_, coverage)| coverage.report_count > 0)
            .map(|(name, coverage)| {
                format!(
                    "{name} aggregate relocation coverage: {} of {} ({}%), floor {floor}%",
                    coverage.diffed_relocations,
                    coverage.total_relocations,
                    coverage.percentage(),
                )
            })
            .collect()
    }

    pub(crate) fn verify(&self, floor: u64) -> Result {
        ensure!(
            self.elf.report_count > 0 || self.macho.report_count > 0,
            "no semantic oracle reports were recorded during the full acceptance run"
        );
        for (name, coverage) in self
            .formats()
            .filter(|(_, coverage)| coverage.report_count > 0)
        {
            let percentage = coverage.percentage();
            ensure!(
                percentage >= floor,
                "{name} aggregate relocation coverage {percentage}% is below the required {floor}% floor"
            );
        }
        Ok(())
    }

    fn formats(&self) -> impl Iterator<Item = (&'static str, &FormatCoverage)> {
        [("ELF", &self.elf), ("Mach-O", &self.macho)].into_iter()
    }
}
