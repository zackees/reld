"""Aggregate every target's benchmark log into the published layout (reld#48).

The per-target measurement steps are already Python. What was left in YAML was the aggregation:
a shell loop that knew the target list, where each raw log lands, how history is carried forward,
and what the published directory looks like. That is the part nobody could run locally, and the
part that silently drifts from `ci.benchmark_stats`' target manifest.

It lives here instead. The workflow installs tools, calls this once, and publishes; this owns the
layout. Nothing here touches credentials or pushes a branch — the publish guard stays in YAML,
where the token is.

Local use, against logs downloaded from a run:

    uv run --no-sync python -m ci.benchmark_pipeline \\
        --input-root benchmark-input --output-root benchmark-stats --no-history-fetch
"""

from __future__ import annotations

import argparse
import shutil
import sys
import urllib.error
from dataclasses import dataclass
from pathlib import Path
from urllib.request import urlopen

from ci.benchmark_stats import BENCHMARK_TARGETS
from ci.benchmark_stats import parse_benchmark_log
from ci.benchmark_stats import read_metadata
from ci.benchmark_stats import write_combined_index
from ci.benchmark_stats import write_outputs

#: What every target directory must contain once rendering is done. Checked rather than assumed:
#: a half-written target directory published to the branch is worse than a failed run.
REQUIRED_ARTIFACTS = ("latest.json", "history.jsonl", "benchmark-link.jpg", "index.html")

DEFAULT_HISTORY_BASE_URL = "https://raw.githubusercontent.com/zackees/reld/benchmark-stats"


class PipelineError(RuntimeError):
    """A failure that must fail the aggregate job, reported with the target it belongs to."""


@dataclass(frozen=True)
class TargetInputs:
    """Where one target's raw measurement landed."""

    target: str
    log: Path
    metadata: Path


def target_inputs(input_root: Path, target: str) -> TargetInputs:
    """Resolve the artifact layout `actions/download-artifact` produces for one target."""
    directory = input_root / f"benchmark-log-{target}"
    return TargetInputs(
        target=target,
        log=directory / "benchmark.log",
        metadata=directory / "metadata.json",
    )


def carry_forward_history(
    target: str,
    destination: Path,
    *,
    history_root: Path | None,
    base_url: str | None,
) -> str:
    """Seed the target's history before rendering appends this run's line.

    History is the one output that cannot be regenerated, so a fetch that fails starts an empty
    file rather than failing the run: losing a line is recoverable, refusing to publish is not.
    """
    destination.parent.mkdir(parents=True, exist_ok=True)
    if history_root is not None:
        source = history_root / target / "history.jsonl"
        if source.is_file():
            shutil.copyfile(source, destination)
            return f"carried forward {source}"
        destination.write_text("", encoding="utf-8")
        return f"no history at {source}; starting empty"
    if base_url:
        url = f"{base_url.rstrip('/')}/{target}/history.jsonl"
        try:
            with urlopen(url, timeout=30) as response:  # noqa: S310 - fixed https base URL
                destination.write_bytes(response.read())
            return f"fetched {url}"
        except (urllib.error.URLError, OSError, ValueError) as error:
            destination.write_text("", encoding="utf-8")
            return f"could not fetch {url} ({error}); starting empty"
    destination.write_text("", encoding="utf-8")
    return "history fetch disabled; starting empty"


def render_target(inputs: TargetInputs, out_dir: Path) -> None:
    """Render one target into its own directory, failing with the target named."""
    if not inputs.log.is_file():
        raise PipelineError(f"{inputs.target}: no raw benchmark log at {inputs.log}")
    if not inputs.metadata.is_file():
        raise PipelineError(f"{inputs.target}: no metadata at {inputs.metadata}")

    report = parse_benchmark_log(inputs.log.read_text(encoding="utf-8"))
    if not report.scenarios() or not report.series:
        raise PipelineError(f"{inputs.target}: {inputs.log} parsed no benchmark rows")

    try:
        meta = read_metadata(inputs.metadata, report)
    except (OSError, ValueError) as error:
        raise PipelineError(f"{inputs.target}: invalid metadata {inputs.metadata}: {error}") from error

    try:
        write_outputs(report, meta, out_dir)
    except SystemExit as error:  # render_jpg refuses an empty chart this way
        raise PipelineError(f"{inputs.target}: {error}") from error


def verify_artifacts(out_dir: Path, target: str) -> None:
    missing = [name for name in REQUIRED_ARTIFACTS if not (out_dir / name).is_file()]
    if missing:
        raise PipelineError(f"{target}: rendering produced no {', '.join(missing)} in {out_dir}")


def run(
    input_root: Path,
    output_root: Path,
    *,
    history_root: Path | None = None,
    base_url: str | None = DEFAULT_HISTORY_BASE_URL,
    targets: tuple[tuple[str, str], ...] = BENCHMARK_TARGETS,
) -> list[str]:
    """Render every target and the combined page. Returns one log line per target."""
    notes: list[str] = []
    for target, _title in targets:
        out_dir = output_root / target
        note = carry_forward_history(
            target,
            out_dir / "history.jsonl",
            history_root=history_root,
            base_url=base_url,
        )
        render_target(target_inputs(input_root, target), out_dir)
        verify_artifacts(out_dir, target)
        notes.append(f"{target}: {note}")
    write_combined_index(output_root)
    return notes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--input-root",
        type=Path,
        required=True,
        help="directory holding benchmark-log-<target>/ from download-artifact",
    )
    parser.add_argument("--output-root", type=Path, required=True, help="published directory to build")
    parser.add_argument(
        "--history-root",
        type=Path,
        help="carry history forward from a local <root>/<target>/history.jsonl instead of the network",
    )
    parser.add_argument(
        "--history-base-url",
        default=DEFAULT_HISTORY_BASE_URL,
        help="base URL containing <target>/history.jsonl",
    )
    parser.add_argument(
        "--no-history-fetch",
        action="store_true",
        help="start every history empty; for local runs with no network",
    )
    args = parser.parse_args(argv)

    base_url = None if (args.no_history_fetch or args.history_root) else args.history_base_url
    try:
        notes = run(
            args.input_root,
            args.output_root,
            history_root=args.history_root,
            base_url=base_url,
        )
    except PipelineError as error:
        sys.stderr.write(f"benchmark aggregation failed: {error}\n")
        return 1

    for note in notes:
        print(note)
    print(f"wrote {args.output_root} for {len(BENCHMARK_TARGETS)} targets")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
