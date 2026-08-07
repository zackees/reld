"""Per-platform benchmark-coverage gate tests (issue #63).

These are the RED -> GREEN gate: a benchmark where an *expected* linker silently reports ``n/a``
must fail the check, and a benchmark where reld is *explicitly pending* on Windows/macOS must
pass. The same log that a pre-#63 pipeline published happily (expected linker as bare ``n/a``) is
now rejected here.
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
| small (16 units) | 0.0210 | 0.0081 | 0.0062 | 0.0044 | 0.0216 |
| large (512 units) | 0.6120 | 0.1602 | 0.0904 | 0.0631 | 0.2100 |
"""

# The exact shape #63 forbids: an expected linker (reld on Linux) as a bare n/a.
LINUX_RELD_NA = LINUX_FULL.replace("| 0.0216 |", "| n/a |").replace("| 0.2100 |", "| n/a |")

# Another silent gap: wild (expected on Linux) drops to n/a.
LINUX_WILD_NA = LINUX_FULL.replace("| 0.0044 |", "| n/a |").replace("| 0.0631 |", "| n/a |")

MACOS_RELD_PENDING = """
## Link Benchmark: aarch64-apple-darwin

| Scenario | ld | ld64.lld | reld |
|:---------|---:|---------:|----:|
| small (16 units) | 0.0300 | 0.0120 | pending |
| large (512 units) | 0.6000 | 0.1600 | pending |
"""

# reld on macOS as a bare n/a — it must be explicitly pending, not silently n/a.
MACOS_RELD_NA = MACOS_RELD_PENDING.replace("| pending |", "| n/a |")

# macOS with ld64.lld silently n/a — the #60 regression the gate must catch.
MACOS_LD64_NA = MACOS_RELD_PENDING.replace("| 0.0120 |", "| n/a |").replace(
    "| 0.1600 |", "| n/a |"
)

WINDOWS_OK = """
## Link Benchmark: x86_64-pc-windows-msvc

| Scenario | link.exe | lld | reld |
|:---------|---------:|----:|----:|
| small (16 units) | 0.0500 | 0.0200 | pending |
"""


def _check(log: str, target: str):
    return check_coverage(parse_benchmark_log(log), target)


def test_policy_matches_issue_63_expected_sets():
    assert expected_for("x86_64-linux") == ["bfd", "lld", "mold", "wild", "reld"]
    assert expected_for("x86_64-pc-windows-msvc") == ["link.exe", "lld"]
    assert expected_for("aarch64-apple-darwin") == ["ld", "ld64.lld"]
    # reld is pending-by-design on Windows/macOS, not on Linux.
    assert "reld" in pending_for("x86_64-pc-windows-msvc")
    assert "reld" in pending_for("aarch64-apple-darwin")
    assert pending_for("x86_64-linux") == {}


def test_full_linux_coverage_passes():
    result = _check(LINUX_FULL, "x86_64-linux")
    assert result.ok, result.violations
    assert result.statuses["reld"] == "measured"


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
    # mold is measured for `small` but silently n/a for `large` — a partial regression that must
    # not slip through just because the series has *some* timing.
    partial = LINUX_FULL.replace("| 0.0904 |", "| n/a |")
    result = _check(partial, "x86_64-linux")
    assert not result.ok
    violation = next((r for linker, r in result.violations if linker == "mold"), None)
    assert violation is not None and "large (512 units)" in violation


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


def test_reld_pending_on_macos_passes():
    result = _check(MACOS_RELD_PENDING, "aarch64-apple-darwin")
    assert result.ok, result.violations
    assert result.statuses["reld"] == "pending"


def test_reld_bare_na_on_macos_fails():
    # reld must be *explicitly* pending on macOS; a bare n/a is the silent gap #63 forbids.
    result = _check(MACOS_RELD_NA, "aarch64-apple-darwin")
    assert not result.ok
    assert any(linker == "reld" for linker, _ in result.violations)


def test_windows_pending_reld_passes():
    result = _check(WINDOWS_OK, "x86_64-pc-windows-msvc")
    assert result.ok, result.violations


def test_target_resolves_via_os_when_label_is_short():
    # A non-canonical label still resolves to a policy through its OS.
    result = _check(LINUX_FULL.replace("x86_64-linux", "linux"), "linux")
    assert result.ok, result.violations


def test_render_summary_lists_roles_and_status():
    report = parse_benchmark_log(MACOS_RELD_PENDING)
    result = check_coverage(report, "aarch64-apple-darwin")
    summary = render_summary(report, result)
    assert "Benchmark coverage: aarch64-apple-darwin" in summary
    assert "ld64.lld" in summary and "expected" in summary
    assert "pending-by-design" in summary
    assert "pending" in summary


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
