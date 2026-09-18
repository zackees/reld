"""Format setup-soldr's cache metrics into a per-target job-summary table.

cross-ship.yml cross-builds reld via the blessed soldr front door and leans on
setup-soldr's caching to keep it fast (reld#59). Nothing measured that caching
before this module existed, so "we lean on the cache" was an assumption
rather than a number.

Every input here is a `setup-soldr` step output, or a wall-clock duration a
workflow step measured itself. GitHub Actions renders an absent step output
as an empty string, not a missing argument, and a future setup-soldr release
can rename or drop an output outright. This module never fabricates a number
to fill the gap: a missing or unparsable value renders as `unknown`, and
nothing here raises for that -- an incomplete cache report must never fail
the job that produced a working cross-build.

Run ``python3 ci/cross_ship_metrics.py --target <name> --triple <triple> ...``
to render a table without a real CI run.
"""

from __future__ import annotations

import argparse
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

UNKNOWN = "unknown"


def normalize(value: str | None) -> str:
    """Return `value` stripped, or UNKNOWN if it is missing or blank.

    GitHub Actions substitutes an absent step output with an empty string
    (`${{ steps.x.outputs.y }}` never errors), so "empty" and "missing" are
    the same case here.
    """
    if value is None:
        return UNKNOWN
    stripped = value.strip()
    return stripped if stripped else UNKNOWN


def format_wall_time(raw_seconds: str | None) -> str:
    """Render a wall-clock duration in seconds as `<minutes>m<seconds>s`.

    Falls back to UNKNOWN for anything that is not a non-negative number,
    rather than guessing at a malformed measurement.
    """
    value = normalize(raw_seconds)
    if value == UNKNOWN:
        return UNKNOWN
    try:
        seconds = float(value)
    except ValueError:
        return UNKNOWN
    if seconds < 0:
        return UNKNOWN
    whole = int(round(seconds))
    minutes, secs = divmod(whole, 60)
    return f"{minutes}m{secs:02d}s"


@dataclass(frozen=True)
class CacheMetrics:
    target: str
    triple: str
    cache_hit: str
    build_cache_hit: str
    target_cache_hit: str
    compile_cache_hits: str
    compile_cache_misses: str
    compile_cache_hit_rate: str
    wall_time: str


TABLE_HEADER = (
    "| target | cache-hit | build-cache-hit | target-cache-hit "
    "| compile-cache-hits | compile-cache-misses | compile-cache-hit-rate | wall time |\n"
    "|---|---|---|---|---|---|---|---|"
)


def format_row(metrics: CacheMetrics) -> str:
    return (
        f"| {metrics.target} ({metrics.triple}) | {metrics.cache_hit} | {metrics.build_cache_hit} "
        f"| {metrics.target_cache_hit} | {metrics.compile_cache_hits} | {metrics.compile_cache_misses} "
        f"| {metrics.compile_cache_hit_rate} | {metrics.wall_time} |"
    )


def format_table(metrics: CacheMetrics) -> str:
    return (
        f"### soldr cache -- {metrics.target} ({metrics.triple})\n\n"
        f"{TABLE_HEADER}\n{format_row(metrics)}\n"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, help="matrix.name, e.g. windows-msvc")
    parser.add_argument("--triple", required=True, help="matrix.target Rust triple")
    parser.add_argument("--cache-hit", default=None)
    parser.add_argument("--build-cache-hit", default=None)
    parser.add_argument("--target-cache-hit", default=None)
    parser.add_argument("--compile-cache-hits", default=None)
    parser.add_argument("--compile-cache-misses", default=None)
    parser.add_argument("--compile-cache-hit-rate", default=None)
    parser.add_argument("--wall-seconds", default=None, help="wall-clock seconds the compile step measured")
    args = parser.parse_args(argv)

    metrics = CacheMetrics(
        target=args.target,
        triple=args.triple,
        cache_hit=normalize(args.cache_hit),
        build_cache_hit=normalize(args.build_cache_hit),
        target_cache_hit=normalize(args.target_cache_hit),
        compile_cache_hits=normalize(args.compile_cache_hits),
        compile_cache_misses=normalize(args.compile_cache_misses),
        compile_cache_hit_rate=normalize(args.compile_cache_hit_rate),
        wall_time=format_wall_time(args.wall_seconds),
    )
    table = format_table(metrics)
    print(table)
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(table)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
