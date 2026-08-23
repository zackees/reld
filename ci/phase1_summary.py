"""Publish deterministic Phase-1 test counts and reference-linker versions."""

from __future__ import annotations

import argparse
import os
import re
from pathlib import Path
from typing import Sequence


RUST_RESULT = re.compile(
    r"test result: (?:ok|FAILED)\. (?P<passed>\d+) passed; "
    r"(?P<failed>\d+) failed; (?P<skip>\d+) ignored"
)
DIFFTEST_RESULT = re.compile(r"(?P<run>\d+) seeds, 0 differential failures")
ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def counts(text: str) -> tuple[int, int]:
    text = ANSI_CSI.sub("", text)
    run = skipped = 0
    for match in RUST_RESULT.finditer(text):
        run += int(match.group("passed")) + int(match.group("failed"))
        skipped += int(match.group("skip"))
    for match in DIFFTEST_RESULT.finditer(text):
        run += int(match.group("run"))
    return run, skipped


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--job", required=True)
    parser.add_argument("--log", action="append", type=Path, required=True)
    parser.add_argument("--versions", type=Path, required=True)
    parser.add_argument("--minimum-run", type=int, default=1)
    parser.add_argument(
        "--upstream-failed",
        choices=("true", "false"),
        default="false",
        help=("allow logs omitted by an earlier failed job step; successful paths must pass false"),
    )
    parser.add_argument(
        "--exact-log-total",
        action="append",
        default=[],
        metavar="PATH=COUNT",
        help="require passed plus failed plus ignored in the named log to equal COUNT",
    )
    args = parser.parse_args(argv)
    upstream_failed = args.upstream_failed == "true"

    exact_totals: dict[Path, int] = {}
    for spec in args.exact_log_total:
        path, separator, value = spec.rpartition("=")
        if not separator or not path:
            raise SystemExit(f"invalid --exact-log-total `{spec}`; expected PATH=COUNT")
        exact_totals[Path(path)] = int(value)

    run = skipped = 0
    missing_logs: list[Path] = []
    for log in args.log:
        if not log.is_file():
            if not upstream_failed:
                raise SystemExit(f"{args.job}: missing expected test log: {log}")
            missing_logs.append(log)
            continue
        current_run, current_skipped = counts(log.read_text(encoding="utf-8", errors="replace"))
        if log in exact_totals:
            actual = current_run + current_skipped
            expected = exact_totals.pop(log)
            if actual != expected and not upstream_failed:
                raise SystemExit(f"{args.job}: {log} registered {actual} tests; expected exactly {expected}")
        run += current_run
        skipped += current_skipped
    if exact_totals and not upstream_failed:
        missing = ", ".join(map(str, exact_totals))
        raise SystemExit(f"{args.job}: exact-total logs were not supplied via --log: {missing}")
    if run < args.minimum_run and not upstream_failed:
        raise SystemExit(f"{args.job}: only {run} tests ran; expected at least {args.minimum_run}")

    if args.versions.is_file():
        versions = args.versions.read_text(encoding="utf-8", errors="replace").strip()
    elif upstream_failed:
        versions = f"unavailable: {args.versions} was not produced"
    else:
        raise SystemExit(f"{args.job}: missing expected versions file: {args.versions}")
    status = " (incomplete: an earlier step failed)" if upstream_failed else ""
    missing_note = ""
    if missing_logs:
        missing_note = f"\nMissing downstream logs: {', '.join(map(str, missing_logs))}\n"
    summary = f"### Phase 1: {args.job}{status}\n\n| Tests run | Tests skipped |\n|---:|---:|\n| {run} | {skipped} |\n{missing_note}\nPinned reference versions:\n\n```text\n{versions}\n```\n"
    print(summary)
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
