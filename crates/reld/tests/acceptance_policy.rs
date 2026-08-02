use reld_core::ensure;
use reld_core::error::Result;
use std::collections::HashSet;

pub(crate) fn should_enforce_aggregate_coverage(args: &libtest_mimic::Arguments) -> bool {
    args.filter.is_none() && args.skip.is_empty() && !args.list && !args.ignored && !args.bench
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
        for pattern in configured_ignores {
            ensure!(
                self.used_ignores.contains(pattern),
                "ignore `{pattern}` is no longer needed; remove it from the fixture"
            );
        }
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
        if self.total_relocations == 0 {
            0
        } else {
            self.diffed_relocations.saturating_mul(100) / self.total_relocations
        }
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
