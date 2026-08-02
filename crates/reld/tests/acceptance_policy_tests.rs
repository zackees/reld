mod acceptance_policy;

use acceptance_policy::AggregateOracleCoverage;
use acceptance_policy::FixtureOracleValidation;
use acceptance_policy::OracleFormat;
use acceptance_policy::TrackedIgnore;
use acceptance_policy::ignore_patterns_for_arch;
use acceptance_policy::should_enforce_aggregate_coverage;
use acceptance_policy::validate_test_config_ignores;

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
fn tracked_ignore_requires_a_positive_issue_on_the_same_line() {
    let tracked = TrackedIgnore::parse_directive("section.data #13").unwrap();
    assert_eq!(
        ignore_patterns_for_arch(&[tracked], "x86_64").unwrap(),
        ["section.data"]
    );

    for invalid in [
        "section.data",
        "section.data #nope",
        "section.data #0",
        "section.data #13 nope",
        "section.data #13 arch=unknown",
        "section.data #13 #14",
        "section.data #nope #13",
    ] {
        assert!(
            TrackedIgnore::parse_directive(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn tracked_ignores_are_scoped_by_architecture_markers() {
    let ignores = [
        TrackedIgnore::parse_directive("rel.R_AARCH64_ABS64 #13").unwrap(),
        TrackedIgnore::parse_directive("segment.RISCV_ATTRIBUTES.* #13").unwrap(),
        TrackedIgnore::parse_directive("riscv_attributes.arch #13").unwrap(),
        TrackedIgnore::parse_directive("dynsym.__global_pointer$.section #13").unwrap(),
        TrackedIgnore::parse_directive("section.data #13").unwrap(),
    ];

    assert_eq!(
        ignore_patterns_for_arch(&ignores, "x86_64").unwrap(),
        ["section.data"]
    );
    assert_eq!(
        ignore_patterns_for_arch(&ignores, "riscv64").unwrap(),
        [
            "segment.RISCV_ATTRIBUTES.*",
            "riscv_attributes.arch",
            "dynsym.__global_pointer$.section",
            "section.data",
        ]
    );
}

#[test]
fn tracked_ignore_can_scope_a_generic_key_to_architectures() {
    let tracked = TrackedIgnore::parse_directive("section.got #13 arch=aarch64,riscv64").unwrap();

    assert!(
        ignore_patterns_for_arch(std::slice::from_ref(&tracked), "x86_64")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ignore_patterns_for_arch(&[tracked], "aarch64").unwrap(),
        ["section.got"]
    );
}

#[test]
fn full_diff_config_rejects_unratcheted_suite_global_ignores() {
    let tracked = TrackedIgnore::parse_directive("section.data #13").unwrap();
    validate_test_config_ignores(false, std::slice::from_ref(&tracked)).unwrap();
    assert!(validate_test_config_ignores(true, &[tracked]).is_err());
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
