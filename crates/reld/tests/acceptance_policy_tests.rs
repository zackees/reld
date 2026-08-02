mod acceptance_policy;

use acceptance_policy::AggregateOracleCoverage;
use acceptance_policy::FixtureOracleValidation;
use acceptance_policy::OracleFormat;
use acceptance_policy::should_enforce_aggregate_coverage;

#[test]
fn aggregate_gate_only_runs_for_a_full_execution() {
    let full = libtest_mimic::Arguments::from_iter(["acceptance"]);
    let list = libtest_mimic::Arguments::from_iter(["acceptance", "--list"]);
    let filtered = libtest_mimic::Arguments::from_iter(["acceptance", "fixture"]);
    let skipped = libtest_mimic::Arguments::from_iter(["acceptance", "--skip", "fixture"]);
    let ignored = libtest_mimic::Arguments::from_iter(["acceptance", "--ignored"]);
    let benches = libtest_mimic::Arguments::from_iter(["acceptance", "--bench"]);

    assert!(should_enforce_aggregate_coverage(&full));
    assert!(!should_enforce_aggregate_coverage(&list));
    assert!(!should_enforce_aggregate_coverage(&filtered));
    assert!(!should_enforce_aggregate_coverage(&skipped));
    assert!(!should_enforce_aggregate_coverage(&ignored));
    assert!(!should_enforce_aggregate_coverage(&benches));
}

#[test]
fn ignore_is_ratcheted_across_all_reports_for_a_fixture() {
    let mut validation = FixtureOracleValidation::default();
    validation.record_used_ignores(["executable-only".to_owned()]);
    validation.record_used_ignores(["shared-object-only".to_owned()]);

    validation
        .verify(&[
            "executable-only".to_owned(),
            "shared-object-only".to_owned(),
        ])
        .unwrap();
    let error = validation
        .verify(&["stale-one".to_owned(), "stale-two".to_owned()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("ignore `stale-one` is no longer needed"));
    assert!(error.contains("ignore `stale-two` is no longer needed"));
}

#[test]
fn relocation_coverage_is_aggregated_instead_of_gated_per_report() {
    let mut coverage = AggregateOracleCoverage::default();
    coverage.record(OracleFormat::Elf, 0, 10);
    coverage.record(OracleFormat::Elf, 10, 10);

    coverage.verify(50).unwrap();
    assert_eq!(
        coverage.summaries(50),
        ["ELF aggregate relocation coverage: 10 of 20 (50%), floor 50%"]
    );
}

#[test]
fn relocation_coverage_keeps_formats_independent() {
    let mut coverage = AggregateOracleCoverage::default();
    coverage.record(OracleFormat::Elf, 1, 1);
    coverage.record(OracleFormat::MachO, 0, 1);

    let error = coverage.verify(50).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Mach-O aggregate relocation coverage 0%")
    );
}

#[test]
fn relocation_coverage_distinguishes_no_format_from_zero_denominator() {
    let mut coverage = AggregateOracleCoverage::default();
    let no_format = coverage.verify(50).unwrap_err();
    assert!(
        no_format
            .to_string()
            .contains("no semantic oracle reports were recorded")
    );
    assert!(coverage.summaries(50).is_empty());

    coverage.record(OracleFormat::Elf, 0, 0);
    let error = coverage.verify(50).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ELF aggregate relocation coverage 0%")
    );
}
