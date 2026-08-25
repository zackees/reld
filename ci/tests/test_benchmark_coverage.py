"""Per-platform benchmark-coverage gate tests (issue #63).

These are the RED -> GREEN gate: a benchmark where an *expected* linker silently reports ``n/a``
must fail the check. Windows/macOS reld now have target-correct bridge front doors, so their
bridge timing is expected just like every other published series.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from ci.benchmark_coverage import (  # noqa: E402
    check_coverage,
    expected_for,
    main,
    pending_for,
    render_summary,
)
from ci.benchmark_stats import parse_benchmark_log  # noqa: E402

LINUX_FULL = """
## Link Benchmark: x86_64-linux

| Scenario | bfd | lld | mold | wild | reld |
|:---------|----:|----:|-----:|-----:|----:|
| no-LTO | 1.2000 | 0.8000 | 0.7000 | 0.6500 | 0.9000 |
| ThinLTO | 1.1000 | 0.7500 | 0.6800 | 0.6200 | 0.8800 |
| full-LTO | 1.0500 | 0.7000 | 0.6600 | 0.6000 | 0.8500 |

## Linker Startup: x86_64-linux

| Linker | Seconds |
|:-------|--------:|
| bfd | 0.0100 |
| lld | 0.0080 |
| mold | 0.0060 |
| wild | 0.0050 |
| reld | 0.0500 |
"""

# The exact shape #63 forbids: an expected linker (reld on Linux) as a bare n/a.
LINUX_RELD_NA = LINUX_FULL.replace("| 0.9000 |", "| n/a |").replace("| 0.8500 |", "| n/a |")

# Another silent gap: wild (expected on Linux) drops to n/a.
LINUX_WILD_NA = LINUX_FULL.replace("| 0.6500 |", "| n/a |").replace("| 0.6000 |", "| n/a |")

MACOS_FULL = """
## Link Benchmark: aarch64-apple-darwin

| Scenario | ld | ld64.lld | reld |
|:---------|---:|---------:|----:|
| no-LTO | 0.9000 | 0.7000 | 0.8000 |
| ThinLTO | 0.8500 | 0.6800 | 0.7800 |
| full-LTO | 0.8000 | 0.6500 | 0.7500 |

## Linker Startup: aarch64-apple-darwin

| Linker | Seconds |
|:-------|--------:|
| ld | 0.0100 |
| ld64.lld | 0.0080 |
| reld | 0.0500 |
"""

# reld on macOS as a bare n/a — a supported bridge cannot be silently missing.
MACOS_RELD_NA = MACOS_FULL.replace("| 0.8000 |", "| n/a |").replace("| 0.7500 |", "| n/a |")

# macOS with ld64.lld silently n/a — the #60 regression the gate must catch.
MACOS_LD64_NA = MACOS_FULL.replace("| 0.7000 |", "| n/a |").replace("| 0.6500 |", "| n/a |")

WINDOWS_FULL = """
## Link Benchmark: x86_64-pc-windows-msvc

| Scenario | link.exe | lld | reld |
|:---------|---------:|----:|----:|
| no-LTO | 0.9000 | 0.7000 | 0.8000 |
| ThinLTO | 0.8500 | 0.6800 | 0.7800 |
| full-LTO | 0.8000 | 0.6500 | 0.7500 |

## Linker Startup: x86_64-pc-windows-msvc

| Linker | Seconds |
|:-------|--------:|
| link.exe | 0.0100 |
| lld | 0.0080 |
| reld | 0.0500 |
"""


def _check(log: str, target: str):
    return check_coverage(parse_benchmark_log(log), target)


def test_policy_matches_issue_63_expected_sets():
    assert expected_for("x86_64-linux") == ["bfd", "lld", "mold", "wild", "reld"]
    assert expected_for("x86_64-pc-windows-msvc") == ["link.exe", "lld", "reld"]
    assert expected_for("aarch64-apple-darwin") == ["ld", "ld64.lld", "reld"]
    assert pending_for("x86_64-linux") == {}
    assert pending_for("x86_64-pc-windows-msvc") == {}
    assert pending_for("aarch64-apple-darwin") == {}


def test_full_linux_coverage_passes():
    result = _check(LINUX_FULL, "x86_64-linux")
    assert result.ok, result.violations
    assert result.statuses["reld"] == "measured"


def test_missing_startup_and_startup_dominated_links_fail_the_gate():
    missing_startup = _check(LINUX_FULL.replace("| reld | 0.0500 |", ""), "x86_64-linux")
    assert any(linker == "reld" and "startup" in reason for linker, reason in missing_startup.violations)

    dominated = _check(LINUX_FULL.replace("| wild | 0.0050 |", "| wild | 0.1000 |"), "x86_64-linux")
    assert any(linker == "<significance>" and "startup-dominated" in reason for linker, reason in dominated.violations)


def test_expected_linker_na_fails_the_gate():
    # This is the RED -> GREEN heart: pre-#63 this log published fine; now it must fail.
    result = _check(LINUX_RELD_NA, "x86_64-linux")
    assert not result.ok
    assert any(linker == "reld" for linker, _ in result.violations)


def test_expected_reference_linker_na_fails_the_gate():
    result = _check(LINUX_WILD_NA, "x86_64-linux")
    assert not result.ok
    assert any(linker == "wild" for linker, _ in result.violations)


def test_partial_scenario_na_for_expected_linker_fails_the_gate():
    # mold is measured for two LTO modes but silently n/a for the third — a partial regression that must
    # not slip through just because the series has *some* timing.
    partial = LINUX_FULL.replace("| 0.6600 |", "| n/a |")
    result = _check(partial, "x86_64-linux")
    assert not result.ok
    violation = next((r for linker, r in result.violations if linker == "mold"), None)
    assert violation is not None and "full-LTO" in violation


def test_lto_configurations_must_be_canonical_complete_and_unique():
    missing = _check(LINUX_FULL.replace("| ThinLTO | 1.1000 | 0.7500 | 0.6800 | 0.6200 | 0.8800 |\n", ""), "x86_64-linux")
    duplicate = _check(LINUX_FULL.replace("| full-LTO", "| ThinLTO"), "x86_64-linux")
    unexpected = _check(LINUX_FULL.replace("no-LTO", "debug"), "x86_64-linux")
    assert any("missing configuration" in reason for _, reason in missing.violations)
    assert any("duplicate configuration" in reason for _, reason in duplicate.violations)
    assert any("unexpected configuration" in reason for _, reason in unexpected.violations)


def test_focused_replay_can_gate_every_linker_without_public_scenario_manifest():
    replay = WINDOWS_FULL.replace("no-LTO", "focused replay")
    replay = "\n".join(line for line in replay.splitlines() if "ThinLTO" not in line and "full-LTO" not in line)
    report = parse_benchmark_log(replay)
    result = check_coverage(
        report,
        "x86_64-pc-windows-msvc",
        require_canonical_scenarios=False,
    )
    assert result.ok, result.violations


def test_render_summary_is_ascii_only():
    # Printed to stdout on the Windows CI leg (cp1252, redirected) — non-ASCII would crash it.
    report = parse_benchmark_log(LINUX_RELD_NA)
    summary = render_summary(report, check_coverage(report, "x86_64-linux"))
    summary.encode("ascii")  # raises UnicodeEncodeError if any emoji slipped back in


def test_macos_ld64_na_fails_the_gate():
    # #60's exact regression: ld64.lld silently n/a must now be loud.
    result = _check(MACOS_LD64_NA, "aarch64-apple-darwin")
    assert not result.ok
    assert any(linker == "ld64.lld" for linker, _ in result.violations)


def test_reld_bridge_on_macos_is_required_and_passes_when_measured():
    result = _check(MACOS_FULL, "aarch64-apple-darwin")
    assert result.ok, result.violations
    assert result.statuses["reld"] == "measured"


def test_reld_bare_na_on_macos_fails():
    result = _check(MACOS_RELD_NA, "aarch64-apple-darwin")
    assert not result.ok
    assert any(linker == "reld" for linker, _ in result.violations)


def test_windows_reld_bridge_is_required_and_passes_when_measured():
    result = _check(WINDOWS_FULL, "x86_64-pc-windows-msvc")
    assert result.ok, result.violations


def test_target_resolves_via_os_when_label_is_short():
    # A non-canonical label still resolves to a policy through its OS.
    result = _check(LINUX_FULL.replace("x86_64-linux", "linux"), "linux")
    assert result.ok, result.violations


def test_render_summary_lists_roles_and_status():
    report = parse_benchmark_log(MACOS_FULL)
    result = check_coverage(report, "aarch64-apple-darwin")
    summary = render_summary(report, result)
    assert "Benchmark coverage: aarch64-apple-darwin" in summary
    assert "ld64.lld" in summary and "expected" in summary
    assert "reld" in summary and "expected" in summary
    assert "measured" in summary


def test_main_exits_nonzero_on_silent_na(tmp_path):
    log = tmp_path / "benchmark.log"
    log.write_text(LINUX_RELD_NA, encoding="utf-8")
    rc = main(["--input-log", str(log), "--target", "x86_64-linux"])
    assert rc == 1


def test_main_exits_zero_on_honest_coverage(tmp_path):
    log = tmp_path / "benchmark.log"
    log.write_text(LINUX_FULL, encoding="utf-8")
    rc = main(["--input-log", str(log), "--target", "x86_64-linux"])
    assert rc == 0
