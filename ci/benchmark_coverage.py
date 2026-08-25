"""Per-platform benchmark-coverage gate.

Makes benchmark coverage *provable and honest* (issue #63): every linker we claim to measure on
a platform must actually produce a real timing, and any remaining gap must be an explicit,
documented **pending** state — never an accidental ``n/a``.

Two policies, both keyed by the benchmark target label:

* ``EXPECTED_LINKERS`` — linkers that MUST produce a real timing. A missing/failed/``pending``
  timing for any of these fails the gate.
* ``PENDING_LINKERS`` — series that are *unsupported-by-design* (documented) on that platform,
  mapped to the reason. These must render as ``pending`` (or be measured); a bare ``n/a`` for one
  of them is itself a failure, because a silent ``n/a`` is exactly what #63 forbids.

Run as a gate::

    python -m ci.benchmark_coverage --input-log path/to/benchmark.log --target x86_64-linux

Exits non-zero (and prints the offending linkers) if coverage is not honest. Always prints a
per-platform expected-vs-measured table to stdout and, when running under Actions, appends it to
``$GITHUB_STEP_SUMMARY`` so a missing linker is loud in the run — not only discoverable by
inspecting the published branch.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from ci.benchmark_stats import (
    CANONICAL_SCENARIOS,
    EXPECTED_SERIES,
    MAX_STARTUP_FRACTION,
    REFERENCE_SERIES,
    Report,
    parse_benchmark_log,
    target_os,
)

# Linkers that MUST produce a real timing on each platform. reld is native on Linux and measured
# through its target-correct lld bridge front door on Windows/macOS. A bridge result is still a
# real result: leaving it pending would let the published chart hide a supported product path.
EXPECTED_LINKERS: dict[str, list[str]] = {target: list(series) for target, series in EXPECTED_SERIES.items()}

# Retained as a distinct policy mechanism for future documented, intentionally unsupported
# series. There are deliberately no permanent benchmark gaps after the bridge measurement slice.
PENDING_LINKERS: dict[str, dict[str, str]] = {}

# Fallback so an unrecognized/short target label still resolves to a policy via its OS.
_OS_TO_TARGET = {
    "Linux": "x86_64-linux",
    "Windows": "x86_64-pc-windows-msvc",
    "Darwin": "aarch64-apple-darwin",
}


def _canonical_target(target: str) -> str | None:
    if target in EXPECTED_LINKERS:
        return target
    os_name = target_os(target)
    return _OS_TO_TARGET.get(os_name or "")


def expected_for(target: str) -> list[str]:
    key = _canonical_target(target)
    return list(EXPECTED_LINKERS.get(key or "", []))


def pending_for(target: str) -> dict[str, str]:
    key = _canonical_target(target)
    return dict(PENDING_LINKERS.get(key or "", {}))


class CoverageResult:
    """Outcome of checking one platform's benchmark report against its policy."""

    def __init__(self, target: str) -> None:
        self.target = target
        # series -> "measured" | "pending" | "na" (only for series we have a policy for)
        self.statuses: dict[str, str] = {}
        # (series, reason) tuples describing each violation.
        self.violations: list[tuple[str, str]] = []

    @property
    def ok(self) -> bool:
        return not self.violations


def check_coverage(report: Report, target: str, *, require_canonical_scenarios: bool = True) -> CoverageResult:
    result = CoverageResult(target)
    expected = expected_for(target)
    pending = pending_for(target)

    if not expected and not pending:
        result.violations.append(("<policy>", f"no coverage policy defined for target {target!r}"))
        return result

    # The charts use one public workload, so its configuration matrix is part of the contract.
    # Report.scenarios() intentionally de-duplicates for rendering; inspect one
    # complete column instead so a repeated markdown row cannot quietly collapse into one bar.
    if not report.series:
        result.violations.append(("<configurations>", "benchmark table has no linker series"))
    elif require_canonical_scenarios:
        scenario_rows = [r.scenario for r in report.rows if r.series == report.series[0]]
        duplicates = sorted({s for s in scenario_rows if scenario_rows.count(s) > 1})
        missing_scenarios = [s for s in CANONICAL_SCENARIOS if s not in scenario_rows]
        unexpected = sorted(set(scenario_rows) - set(CANONICAL_SCENARIOS))
        if duplicates:
            result.violations.append(("<configurations>", f"duplicate configuration(s): {', '.join(duplicates)}"))
        if missing_scenarios:
            result.violations.append(("<configurations>", f"missing configuration(s): {', '.join(missing_scenarios)}"))
        if unexpected:
            result.violations.append(("<configurations>", f"unexpected configuration(s): {', '.join(unexpected)}"))

    for linker in expected:
        status = report.series_status(linker)
        result.statuses[linker] = status
        if status != "measured":
            detail = "reported n/a (failed/unavailable)" if status == "na" else f"reported {status}"
            result.violations.append((linker, f"expected linker {linker!r} must be measured but {detail}"))
            continue
        # Measured overall, but an expected linker must produce a real timing for *every*
        # scenario — a partial n/a is a silent regression, exactly what #63 forbids.
        missing = [r.scenario for r in report.rows if r.series == linker and r.seconds is None]
        if missing:
            result.violations.append(
                (
                    linker,
                    f"expected linker {linker!r} has no timing for scenario(s): " f"{', '.join(missing)}",
                )
            )

        startup = report.startup_seconds.get(linker)
        if startup is None or startup <= 0:
            result.violations.append((linker, f"expected linker {linker!r} has no measured startup timing"))

    for linker, reason in pending.items():
        status = report.series_status(linker)
        result.statuses.setdefault(linker, status)
        # A pending-by-design series may be measured (bonus) or pending, but a bare n/a means the
        # runner did not emit the deliberate pending marker — the silent gap #63 forbids.
        if status == "na":
            result.violations.append(
                (
                    linker,
                    f"pending-by-design linker {linker!r} reported a bare n/a; it must be marked " f"pending ({reason})",
                )
            )

    canonical_target = _canonical_target(target)
    if canonical_target and report.startup_seconds:
        largest_startup = max(report.startup_seconds.values())
        reference = REFERENCE_SERIES[canonical_target]
        for scenario in CANONICAL_SCENARIOS:
            reference_seconds = report.value(scenario, reference)
            if reference_seconds is None or reference_seconds <= 0:
                continue
            fraction = largest_startup / reference_seconds
            if fraction > MAX_STARTUP_FRACTION:
                result.violations.append(
                    (
                        "<significance>",
                        f"{scenario} is startup-dominated: largest startup is {fraction:.1%} "
                        f"of {reference} final link (limit {MAX_STARTUP_FRACTION:.0%})",
                    )
                )

    return result


def render_summary(report: Report, result: CoverageResult) -> str:
    expected = set(expected_for(result.target))
    pending = pending_for(result.target)

    def role(linker: str) -> str:
        if linker in expected:
            return "expected"
        if linker in pending:
            return "pending-by-design"
        return "extra"

    lines = [
        f"### Benchmark coverage: {result.target}",
        "",
        "| Linker | Role | Status |",
        "|:-------|:-----|:-------|",
    ]
    # Cover every series present plus every policy linker, in a stable order.
    ordered = list(dict.fromkeys([*expected_for(result.target), *pending, *report.series]))
    for linker in ordered:
        status = result.statuses.get(linker) or report.series_status(linker)
        # ASCII only: this string is also printed to stdout, which on the Windows CI leg is a
        # redirected (non-tty) stream using the cp1252 locale encoding — emoji would raise
        # UnicodeEncodeError and crash the gate even when coverage is healthy.
        mark = {"measured": "OK measured", "pending": "-- pending", "na": "XX n/a"}.get(status, status)
        lines.append(f"| {linker} | {role(linker)} | {mark} |")

    lines.append("")
    if result.ok:
        lines.append("All expected linkers measured; no silent `n/a`.")
    else:
        lines.append("**Coverage gate failed:**")
        for linker, reason in result.violations:
            lines.append(f"- `{linker}`: {reason}")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="ci.benchmark_coverage")
    p.add_argument("--input-log", type=Path, required=True)
    p.add_argument(
        "--target",
        default=None,
        help="benchmark target label (e.g. x86_64-linux); falls back to the log heading",
    )
    p.add_argument(
        "--allow-noncanonical-scenarios",
        action="store_true",
        help=("check linker coverage for a focused smoke/replay table without requiring the " "public no-LTO/ThinLTO/full-LTO configuration manifest"),
    )
    args = p.parse_args(argv)

    report = parse_benchmark_log(args.input_log.read_text(encoding="utf-8"))
    # Prefer the explicit target; fall back to the label the runner stamped in the log.
    target = args.target or report.label
    if not target:
        sys.stderr.write("no --target given and no benchmark heading found in the log\n")
        return 1

    result = check_coverage(
        report,
        target,
        require_canonical_scenarios=not args.allow_noncanonical_scenarios,
    )
    summary = render_summary(report, result)
    print(summary)
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(summary)

    if not result.ok:
        sys.stderr.write(f"benchmark coverage gate failed for {target}: " + "; ".join(reason for _, reason in result.violations) + "\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
