"""Parse reld-bench markdown output and render the published benchmark chart.

The runner prints final-link medians under ``## Link Benchmark:`` and raw, unsubtracted fixed
startup medians under ``## Linker Startup:``. Public schema and history identifiers change when
the workload generation changes so incomparable results cannot be mixed.

Outputs (all into ``--output-dir``):

* ``benchmark-link.jpg`` -- the image embedded in the README
* ``latest.json``        -- machine-readable results for the most recent run
* ``history.jsonl``      -- one appended line per run, capped
* ``index.html``         -- self-contained page for GitHub Pages
* ``.nojekyll``          -- so Pages serves the files verbatim

Run ``python -m ci.benchmark_stats --input-log path`` to iterate on chart layout without
running a real benchmark.
"""

from __future__ import annotations

import argparse
import calendar
import json
import os
import platform
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.request import urlopen

SCHEMA_VERSION = 6
BENCHMARK_ID = "artifact-auditor-native-linux-lto-v2"
HISTORY_MAX_LINES = 1000
IMAGE_NAME = "benchmark-link.jpg"
HEADING_PREFIX = "## Link Benchmark:"
STARTUP_HEADING_PREFIX = "## Linker Startup:"

# One canonical manifest for the README, workflow aggregation, and generated artifacts. Keeping
# this in the renderer makes a target added to one consumer but not another a testable drift,
# rather than a quietly stale README panel.
BENCHMARK_TARGETS = (
    ("x86_64-linux", "Linux"),
    ("x86_64-pc-windows-msvc", "Windows"),
    ("aarch64-apple-darwin", "macOS"),
)

# The public workload/configuration matrix is deliberately stable. Missing, duplicate, or renamed
# rows are incomparable to history and must never reach the published branch.
CANONICAL_SCENARIOS = (
    "no-LTO",
    "ThinLTO",
    "full-LTO",
)

# Shared public-artifact contract for the coverage gate and remote freshness watchdog.
EXPECTED_SERIES = {
    "x86_64-linux": ("bfd", "lld", "mold", "wild", "reld"),
    "x86_64-pc-windows-msvc": ("link.exe", "lld", "reld"),
    "aarch64-apple-darwin": ("ld", "ld64.lld", "reld"),
}
REFERENCE_SERIES = {
    "x86_64-linux": "wild",
    "x86_64-pc-windows-msvc": "lld",
    "aarch64-apple-darwin": "ld64.lld",
}
MAX_STARTUP_FRACTION = 0.10

# Rendered at SCALE x then downsampled; cheap supersampling beats fighting PIL's aliasing.
WIDTH = 900
SCALE = 4

BG = (13, 17, 23)
PANEL = (22, 27, 34)
FG = (230, 237, 243)
MUTED = (125, 133, 144)
GRID = (48, 54, 61)
NA = (70, 76, 86)
# Pending (unsupported-by-design) reads as a deliberate, documented gap — amber, distinct from the
# dim grey of a failed ``n/a`` so the chart never conflates the two.
PENDING = (187, 128, 9)

# Stable colour per linker so a series keeps its identity across runs.
SERIES_COLORS = {
    "reld": (88, 166, 255),
    "wild": (63, 185, 80),
    "mold": (210, 153, 34),
    "lld": (163, 113, 247),
    "bfd": (248, 81, 73),
}
FALLBACK_COLOR = (139, 148, 158)


@dataclass
class Row:
    scenario: str
    series: str
    seconds: float | None
    # A cell is *pending* when the linker is unsupported-by-design on this platform (documented,
    # e.g. reld's bridge measurement on Windows/macOS) rather than *failed* (a real ``n/a``). The
    # two are indistinguishable as bare ``n/a`` — this flag is what keeps them apart downstream.
    pending: bool = False

    def status(self) -> str:
        if self.seconds is not None:
            return "measured"
        return "pending" if self.pending else "na"


@dataclass
class Report:
    label: str = ""
    series: list[str] = field(default_factory=list)
    rows: list[Row] = field(default_factory=list)
    startup_seconds: dict[str, float] = field(default_factory=dict)

    def scenarios(self) -> list[str]:
        seen: list[str] = []
        for r in self.rows:
            if r.scenario not in seen:
                seen.append(r.scenario)
        return seen

    def value(self, scenario: str, series: str) -> float | None:
        for r in self.rows:
            if r.scenario == scenario and r.series == series:
                return r.seconds
        return None

    def is_pending(self, scenario: str, series: str) -> bool:
        for r in self.rows:
            if r.scenario == scenario and r.series == series:
                return r.pending
        return False

    def series_status(self, series: str) -> str:
        """Collapse a series' per-scenario cells into one status. A series counts as *measured*
        if any scenario produced a real timing, *pending* if it was never measured but every
        cell is a deliberate pending marker, otherwise *na* (failed)."""
        cells = [r for r in self.rows if r.series == series]
        if any(r.seconds is not None for r in cells):
            return "measured"
        if cells and all(r.pending for r in cells):
            return "pending"
        return "na"


def _clean(cell: str) -> str:
    return cell.replace("**", "").replace("`", "").strip()


def _to_seconds(cell: str) -> float | None:
    cell = _clean(cell).lower()
    if cell in ("", "n/a", "na", "-", "—"):
        return None
    try:
        return float(cell.rstrip("s"))
    except ValueError:
        return None


# Cell tokens the runner uses to mark a linker it did not time on purpose, rather than one that
# failed. Kept distinct from the ``n/a`` family in ``_to_seconds`` so pending never renders as a
# silent failure.
_PENDING_TOKENS = ("pending", "pending*", "todo")


def _classify(cell: str) -> tuple[float | None, bool]:
    """Return ``(seconds, pending)`` for a raw table cell. ``seconds`` is ``None`` for both
    failed (``n/a``) and pending cells; ``pending`` is ``True`` only for the deliberate markers."""
    if _clean(cell).lower() in _PENDING_TOKENS:
        return None, True
    return _to_seconds(cell), False


def parse_benchmark_log(text: str) -> Report:
    """Scrape final-link and separate startup tables out of one runner log."""
    report = Report()
    table_kind = ""
    header: list[str] = []

    # A UTF-8 BOM ahead of the first heading otherwise makes startswith() miss and the whole
    # log parse silently to zero rows.
    text = text.lstrip("﻿")

    for raw in text.splitlines():
        line = raw.strip().lstrip("﻿")

        if line.startswith(HEADING_PREFIX):
            report.label = line[len(HEADING_PREFIX) :].strip()
            table_kind = "links"
            header = []
            continue
        if line.startswith(STARTUP_HEADING_PREFIX):
            startup_label = line[len(STARTUP_HEADING_PREFIX) :].strip()
            if report.label and startup_label != report.label:
                raise ValueError(f"startup target {startup_label!r} does not match {report.label!r}")
            report.label = report.label or startup_label
            table_kind = "startup"
            header = []
            continue

        if not table_kind:
            continue

        if not line.startswith("|"):
            # A blank line inside the table block is tolerated; anything else ends it.
            if line == "" or line.startswith("<!--"):
                continue
            table_kind = ""
            continue

        cells = [_clean(c) for c in line.strip("|").split("|")]
        if not header:
            header = cells
            if table_kind == "links":
                report.series = [c for c in cells[1:] if c]
            continue
        if set("".join(cells)) <= set("-: "):
            continue  # the |---|---| separator row

        scenario = cells[0]
        if not scenario:
            continue
        if table_kind == "startup":
            value = _to_seconds(cells[1]) if len(cells) > 1 else None
            if value is not None:
                report.startup_seconds[scenario] = value
            continue
        for idx, series in enumerate(report.series, start=1):
            value, pending = _classify(cells[idx]) if idx < len(cells) else (None, False)
            report.rows.append(Row(scenario=scenario, series=series, seconds=value, pending=pending))

    return report


def target_os(target: str) -> str | None:
    """Infer the benchmark OS from its stable target label when available."""
    normalized = target.lower()
    if "windows" in normalized or "msvc" in normalized:
        return "Windows"
    if "darwin" in normalized or "macos" in normalized:
        return "Darwin"
    if "linux" in normalized:
        return "Linux"
    return None


def series_mode(series: str, runner_os: str, target: str = "") -> str:
    """How a benchmark series was produced. reld is the subject; everything else is a
    reference linker. reld links natively (wild ELF) on Linux and via the lld bridge
    elsewhere."""
    if series == "reld":
        effective_os = target_os(target) or runner_os
        return "native" if effective_os == "Linux" else "bridge"
    return "reference"


def series_engine(series: str, runner_os: str, target: str = "") -> str:
    """Concrete linker engine behind a series.

    `reld` is a native ELF engine on Linux and deliberately routes its COFF/Mach-O front doors
    to lld.  Persisting this beside ``mode`` prevents a bridge measurement from being mistaken
    for native COFF/Mach-O throughput by a JSON or chart consumer.
    """
    if series != "reld":
        return series
    if series_mode(series, runner_os, target) == "native":
        return "reld"
    effective_os = target_os(target) or runner_os
    if effective_os == "Windows":
        return "lld-link"
    if effective_os == "Darwin":
        return "ld64.lld"
    return "lld"


def series_label(series: str, runner_os: str, target: str = "") -> str:
    """Human-facing series label that makes reld's execution mode visible in charts."""
    if series != "reld":
        return series
    mode = series_mode(series, runner_os, target)
    engine = series_engine(series, runner_os, target)
    return f"reld ({mode}/{engine})"


def _tool_version(cmd: list[str]) -> str | None:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return None
    blob = (out.stdout or out.stderr or "").strip()
    return blob.splitlines()[0] if blob else None


def collect_metadata(report: Report) -> dict[str, Any]:
    repo = os.environ.get("GITHUB_REPOSITORY", "zackees/reld")
    raw_base = os.environ.get(
        "RELD_BENCHMARK_RAW_BASE_URL",
        f"https://raw.githubusercontent.com/{repo}/benchmark-stats",
    )
    run_id = os.environ.get("GITHUB_RUN_ID")
    return {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "repository": repo,
        "git_sha": os.environ.get("GITHUB_SHA", ""),
        "git_ref": os.environ.get("GITHUB_REF", ""),
        "run_url": f"https://github.com/{repo}/actions/runs/{run_id}" if run_id else None,
        "target": report.label,
        "benchmark": {
            "id": BENCHMARK_ID,
            "workload": "ci/e2e/link-workload",
            "configurations": list(CANONICAL_SCENARIOS),
        },
        "runner": {
            "os": target_os(report.label) or platform.system(),
            "arch": platform.machine(),
            "platform": platform.platform(),
            "cpu_count": os.cpu_count(),
        },
        "versions": {
            "clang": _tool_version(["clang", "--version"]),
            "mold": _tool_version(["mold", "--version"]),
            "wild": _tool_version(["wild", "--version"]),
            "ld.lld": _tool_version(["ld.lld", "--version"]),
        },
        "raw_image_base_url": raw_base,
        "raw_image_url": f"{raw_base}/{report.label}/{IMAGE_NAME}",
        "pages_url": os.environ.get("RELD_BENCHMARK_PAGES_URL"),
    }


def read_metadata(path: Path, report: Report) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError("metadata root is not an object")
    if payload.get("target") != report.label:
        raise ValueError(f"metadata target {payload.get('target')!r} does not match {report.label!r}")
    return payload


def render_summary(report: Report, meta: dict[str, Any], publish_outcome: str = "rendered") -> str:
    """Render the concise, per-target Actions/log report required to diagnose publication.

    It intentionally derives statuses from every scenario, not only a series-level success: an
    expected linker with one missing timing remains visibly incomplete before coverage rejects it.
    """
    scenarios = report.scenarios()
    headings = " | ".join(scenarios) if scenarios else "(none parsed)"
    lines = [
        f"### Benchmark report: {meta.get('target') or report.label or 'unknown'}",
        "",
        "| Field | Value |",
        "|:------|:------|",
        f"| source SHA | {meta.get('git_sha') or 'local/unset'} |",
        f"| generated at | {meta.get('generated_at') or 'unknown'} |",
        f"| publish outcome | {publish_outcome} |",
        "",
        f"Timings by configuration: {headings}",
        "",
        "| Series | Mode | Engine | Status | Timings (seconds) |",
        "|:-------|:-----|:-------|:-------|:------------------|",
    ]
    runner_os = meta.get("runner", {}).get("os", "")
    target = meta.get("target", report.label)
    for series in report.series:
        cells: list[str] = []
        rows = [r for r in report.rows if r.series == series]
        for row in rows:
            if row.seconds is not None:
                cells.append(f"{row.scenario}: {row.seconds:.4f}")
            else:
                cells.append(f"{row.scenario}: {'pending' if row.pending else 'n/a'}")
        lines.append(f"| {series} | {series_mode(series, runner_os, target)} | {series_engine(series, runner_os, target)} | {report.series_status(series)} | {' ; '.join(cells) or 'n/a'} |")
    lines.extend(
        [
            "",
            "Fixed linker startup (reported raw; never subtracted)",
            "",
            "| Series | Startup (seconds) |",
            "|:-------|------------------:|",
            *(f"| {series} | {report.startup_seconds.get(series, float('nan')):.4f} |" for series in report.series),
        ]
    )
    return "\n".join(lines) + "\n"


def render_readme_block() -> str:
    """Generate the entire README benchmark region, not merely its image panels."""
    panels = []
    for target, title in BENCHMARK_TARGETS:
        panels.append(
            f'<p align="center"><b>{target}</b><br>\n'
            f'<a href="https://github.com/zackees/reld/tree/benchmark-stats/{target}"><img '
            f'alt="{title} reld link benchmark" '
            f'src="https://raw.githubusercontent.com/zackees/reld/benchmark-stats/{target}/'
            f'benchmark-link.jpg" width="100%"></a></p>'
        )
    prose = (
        "*Auto-generated nightly by [`benchmark-stats.yml`](.github/workflows/benchmark-stats.yml) and\n"
        "published to the [`benchmark-stats` branch](https://github.com/zackees/reld/tree/benchmark-stats),\n"
        "with independent `latest.json` and `history.jsonl` per target. Each chart links the same\n"
        "idiomatic, moderately link-heavy Rust artifact-auditing project in `no-LTO`, `ThinLTO`, and `full-LTO`\n"
        "configurations; compilation happens once per configuration and only the captured final link\n"
        "is timed. Fixed linker startup is measured and reported separately, never subtracted, and a\n"
        "10% significance gate prevents startup-dominated results. Linux measures reld's native\n"
        "engine; Windows and macOS measure reld through their target-correct `lld` **bridge** front doors.\n"
        "`latest.json` records both the series `mode` (`native` or `bridge`) and concrete `engine` (`reld`\n"
        "on Linux, `lld-link` on Windows, `ld64.lld` on macOS), and the charts label bridge results so they\n"
        "are never presented as native COFF/Mach-O throughput. Each platform gates its **expected** linkers\n"
        "in CI (Linux `bfd`/`lld`/`mold`/`wild`/`reld`, Windows `link.exe`/`lld`/`reld`, macOS\n"
        "`ld`/`ld64.lld`/`reld`): a missing timing or a missing/duplicate/unexpected configuration\n"
        "fails the build, so coverage can never silently understate itself (see\n"
        "[#63](https://github.com/zackees/reld/issues/63)). The generated artifact freshness guard also\n"
        "requires every published chart to name the source SHA and current generation time.*"
    )
    return "\n\n".join([*panels, prose]) + "\n"


def check_readme_block(path: Path) -> bool:
    """Return whether README's generated benchmark section exactly matches the manifest."""
    text = path.read_text(encoding="utf-8")
    begin = "<!-- BENCHMARK:BEGIN -->"
    end = "<!-- BENCHMARK:END -->"
    if text.count(begin) != 1 or text.count(end) != 1:
        return False
    actual = text.split(begin, 1)[1].split(end, 1)[0].strip()
    return actual == render_readme_block().strip()


def write_readme_block(path: Path) -> None:
    """Replace exactly the marker-delimited README section with canonical contents."""
    text = path.read_text(encoding="utf-8")
    begin = "<!-- BENCHMARK:BEGIN -->"
    end = "<!-- BENCHMARK:END -->"
    if text.count(begin) != 1 or text.count(end) != 1:
        raise ValueError("README must contain exactly one BENCHMARK:BEGIN and BENCHMARK:END marker")
    before, remainder = text.split(begin, 1)
    _, after = remainder.split(end, 1)
    path.write_text(f"{before}{begin}\n{render_readme_block()}{end}{after}", encoding="utf-8")


def validate_startup_and_significance(payload: dict[str, Any], target: str) -> list[str]:
    """Validate startup coverage and recompute the public significance invariant."""
    errors: list[str] = []
    startup = payload.get("startup")
    if not isinstance(startup, list):
        return [f"{target}: startup is missing or not a list"]
    cells: dict[str, dict[str, Any]] = {}
    for entry in startup:
        if not isinstance(entry, dict):
            errors.append(f"{target}: startup entry is not an object")
            continue
        series = str(entry.get("series", ""))
        if series in cells:
            errors.append(f"{target}: duplicate startup result for {series!r}")
        cells[series] = entry
    for series in EXPECTED_SERIES[target]:
        entry = cells.get(series)
        if entry is None:
            errors.append(f"{target}: {series} startup is missing")
            continue
        seconds = entry.get("seconds")
        if entry.get("status") != "measured" or not isinstance(seconds, (int, float)) or seconds <= 0:
            errors.append(f"{target}: {series} startup is not measured")
        expected_mode = series_mode(series, "", target)
        expected_engine = series_engine(series, "", target)
        if entry.get("mode") != expected_mode:
            errors.append(f"{target}: {series} startup mode {entry.get('mode')!r} != {expected_mode!r}")
        if entry.get("engine") != expected_engine:
            errors.append(f"{target}: {series} startup engine {entry.get('engine')!r} != {expected_engine!r}")
    unexpected = sorted(set(cells) - set(EXPECTED_SERIES[target]))
    if unexpected:
        errors.append(f"{target}: unexpected startup series: {', '.join(unexpected)}")

    numeric_startups = [entry.get("seconds") for entry in cells.values() if isinstance(entry.get("seconds"), (int, float))]
    results = payload.get("results")
    if numeric_startups and isinstance(results, list):
        largest = max(numeric_startups)
        reference = REFERENCE_SERIES[target]
        for scenario in CANONICAL_SCENARIOS:
            reference_cell = next(
                (entry for entry in results if isinstance(entry, dict) and entry.get("configuration") == scenario and entry.get("series") == reference),
                None,
            )
            seconds = reference_cell.get("seconds") if reference_cell else None
            if not isinstance(seconds, (int, float)) or seconds <= 0:
                continue
            fraction = largest / seconds
            if fraction > MAX_STARTUP_FRACTION:
                errors.append(f"{target}: {scenario} is startup-dominated ({fraction:.1%}; limit {MAX_STARTUP_FRACTION:.0%})")
    return errors


def verify_current_outputs(out_root: Path, expected_sha: str, max_age_seconds: int) -> list[str]:
    """Validate local generated artifacts before they are force-published.

    A successful force-push of this tree therefore cannot publish a chart from a different source
    SHA or an accidentally reused, stale renderer directory.
    """
    errors: list[str] = []
    now = time.time()
    for target, _ in BENCHMARK_TARGETS:
        latest = out_root / target / "latest.json"
        if not latest.is_file():
            errors.append(f"{target}: latest.json is missing")
            continue
        try:
            payload = json.loads(latest.read_text(encoding="utf-8"))
            meta = payload["metadata"]
            stamp = time.strptime(meta["generated_at"], "%Y-%m-%dT%H:%M:%SZ")
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            errors.append(f"{target}: invalid latest.json metadata ({error})")
            continue
        if payload.get("schema_version") != SCHEMA_VERSION:
            errors.append(f"{target}: schema version {payload.get('schema_version')!r} != {SCHEMA_VERSION}")
        if payload.get("benchmark_id") != BENCHMARK_ID:
            errors.append(f"{target}: benchmark id {payload.get('benchmark_id')!r} != {BENCHMARK_ID!r}")
        if meta.get("target") != target:
            errors.append(f"{target}: metadata target is {meta.get('target')!r}")
        if expected_sha and meta.get("git_sha") != expected_sha:
            errors.append(f"{target}: source SHA {meta.get('git_sha')!r} != {expected_sha!r}")
        errors.extend(validate_startup_and_significance(payload, target))
        age = now - calendar.timegm(stamp)
        if age < -300 or age > max_age_seconds:
            errors.append(f"{target}: generated timestamp is {age:.0f}s old (limit {max_age_seconds}s)")
    return errors


def _parse_timestamp(value: object) -> float:
    if not isinstance(value, str):
        raise ValueError("generated_at is absent or not a string")
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()


def _fetch_json(url: str) -> dict[str, Any]:
    with urlopen(url, timeout=30) as response:  # nosec B310: explicit workflow/test input
        payload = json.loads(response.read().decode("utf-8"))
    if not isinstance(payload, dict):
        raise ValueError("JSON root is not an object")
    return payload


def verify_remote_freshness(
    base_url: str,
    max_age_seconds: int,
    *,
    expected_sha: str = "",
    fetch_json: Any = _fetch_json,
    now: float | None = None,
) -> list[str]:
    """Validate every public ``latest.json`` without depending on the aggregate job.

    Both the base URL and fetcher are injectable to make the watchdog unit-testable against local
    fixture data. This job intentionally has no dependency on benchmark/aggregate, so a failed
    nightly benchmark cannot conceal an old or incomplete public branch.
    """
    errors: list[str] = []
    checked_at = time.time() if now is None else now
    root = base_url.rstrip("/")
    for target, _ in BENCHMARK_TARGETS:
        url = f"{root}/{target}/latest.json"
        try:
            payload = fetch_json(url)
            if payload.get("schema_version") != SCHEMA_VERSION:
                errors.append(f"{target}: schema version {payload.get('schema_version')!r} != {SCHEMA_VERSION}")
            if payload.get("benchmark_id") != BENCHMARK_ID:
                errors.append(f"{target}: benchmark id {payload.get('benchmark_id')!r} != {BENCHMARK_ID!r}")
            meta = payload["metadata"]
            if not isinstance(meta, dict):
                raise ValueError("metadata is not an object")
            age = checked_at - _parse_timestamp(meta.get("generated_at"))
            if age < -300 or age > max_age_seconds:
                errors.append(f"{target}: latest.json is {age:.0f}s old (limit {max_age_seconds}s)")
            if meta.get("target") != target:
                errors.append(f"{target}: metadata target is {meta.get('target')!r}")
            if expected_sha and meta.get("git_sha") != expected_sha:
                errors.append(f"{target}: source SHA {meta.get('git_sha')!r} != {expected_sha!r}")
            errors.extend(validate_startup_and_significance(payload, target))
            results = payload.get("results")
            if not isinstance(results, list):
                errors.append(f"{target}: results is missing or not a list")
                continue
            cells: dict[tuple[str, str], dict[str, Any]] = {}
            for result in results:
                if not isinstance(result, dict):
                    errors.append(f"{target}: result entry is not an object")
                    continue
                key = (
                    str(result.get("configuration", "")),
                    str(result.get("series", "")),
                )
                if key in cells:
                    errors.append(f"{target}: duplicate result for {key[0]!r}/{key[1]!r}")
                cells[key] = result
            actual_scenarios = {scenario for scenario, _ in cells}
            unexpected = sorted(actual_scenarios - set(CANONICAL_SCENARIOS))
            if unexpected:
                errors.append(f"{target}: unexpected configuration(s): {', '.join(unexpected)}")
            for series in EXPECTED_SERIES[target]:
                for scenario in CANONICAL_SCENARIOS:
                    cell = cells.get((scenario, series))
                    if cell is None:
                        errors.append(f"{target}: {series} missing {scenario}")
                    elif cell.get("status") != "measured" or cell.get("seconds") is None:
                        errors.append(f"{target}: {series} {scenario} is not measured")
                    else:
                        expected_mode = series_mode(series, "", target)
                        expected_engine = series_engine(series, "", target)
                        if cell.get("mode") != expected_mode:
                            errors.append(f"{target}: {series} {scenario} mode {cell.get('mode')!r} != {expected_mode!r}")
                        if cell.get("engine") != expected_engine:
                            errors.append(f"{target}: {series} {scenario} engine {cell.get('engine')!r} != {expected_engine!r}")
        except Exception as error:  # malformed/unreachable remote artifacts are freshness failures
            errors.append(f"{target}: cannot validate {url}: {error}")
    return errors


def _font(size: int):
    from PIL import ImageFont

    candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ]
    for path in candidates:
        if Path(path).exists():
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
    return ImageFont.load_default()


def render_jpg(report: Report, meta: dict[str, Any], out_path: Path) -> None:
    from PIL import Image, ImageDraw

    scenarios = report.scenarios()
    series = report.series
    if not scenarios or not series:
        raise SystemExit("no benchmark rows parsed - refusing to render an empty chart")

    row_h = 26
    group_h = len(series) * row_h + 34
    header_h = 74
    startup_h = 52
    footer_h = 34
    height = header_h + len(scenarios) * group_h + startup_h + footer_h

    img = Image.new("RGB", (WIDTH * SCALE, height * SCALE), BG)
    d = ImageDraw.Draw(img)
    f_title = _font(19 * SCALE)
    f_meta = _font(11 * SCALE)
    f_label = _font(12 * SCALE)

    d.rectangle([0, 0, WIDTH * SCALE, header_h * SCALE], fill=PANEL)
    d.text((20 * SCALE, 16 * SCALE), "reld - link benchmark", font=f_title, fill=FG)
    sha = (meta.get("git_sha") or "")[:12]
    sub = f"{meta['generated_at']}  |  {meta.get('target', '')}  |  {meta['runner']['platform']}"
    if sha:
        sub += f"  |  sha {sha}"
    d.text((20 * SCALE, 46 * SCALE), sub, font=f_meta, fill=MUTED)

    label_w = 150
    bar_x = label_w + 30
    bar_max = WIDTH - bar_x - 90

    y = header_h
    for scenario in scenarios:
        d.text((20 * SCALE, (y + 8) * SCALE), scenario, font=f_label, fill=FG)
        y += 26

        values = [report.value(scenario, s) for s in series]
        present = [v for v in values if v is not None]
        peak = max(present) if present else 1.0

        for s, v in zip(series, values):
            color = SERIES_COLORS.get(s, FALLBACK_COLOR)
            d.text(
                (34 * SCALE, (y + 5) * SCALE),
                series_label(s, meta["runner"]["os"], meta.get("target", "")),
                font=f_meta,
                fill=MUTED,
            )

            if v is None:
                pending = report.is_pending(scenario, s)
                edge = PENDING if pending else NA
                label = "pending" if pending else "n/a"
                d.rectangle(
                    [bar_x * SCALE, (y + 4) * SCALE, (bar_x + 60) * SCALE, (y + 18) * SCALE],
                    outline=edge,
                    width=SCALE,
                )
                d.text(((bar_x + 70) * SCALE, (y + 5) * SCALE), label, font=f_meta, fill=edge)
            else:
                w = max(2, int(bar_max * (v / peak))) if peak > 0 else 2
                d.rectangle(
                    [bar_x * SCALE, (y + 4) * SCALE, (bar_x + w) * SCALE, (y + 18) * SCALE],
                    fill=color,
                )
                d.text(
                    ((bar_x + w + 10) * SCALE, (y + 5) * SCALE),
                    f"{v:.4f}s",
                    font=f_meta,
                    fill=FG,
                )
            y += row_h

        d.line([(20 * SCALE, y * SCALE), ((WIDTH - 20) * SCALE, y * SCALE)], fill=GRID, width=SCALE)
        y += 8

    startup_text = "  |  ".join(f"{series_label(series, meta['runner']['os'], meta.get('target', ''))}: {seconds:.4f}s" for series, seconds in report.startup_seconds.items())
    d.text((20 * SCALE, (y + 6) * SCALE), "fixed linker startup (not subtracted)", font=f_meta, fill=FG)
    d.text((20 * SCALE, (y + 25) * SCALE), startup_text or "startup data missing", font=f_meta, fill=MUTED)

    d.text(
        (20 * SCALE, (height - 24) * SCALE),
        "lower is better  |  median of N trials, link step only  |  n/a = linker failed/unavailable  |  pending = unsupported-by-design (documented)",
        font=f_meta,
        fill=MUTED,
    )

    img = img.resize((WIDTH, height), Image.Resampling.LANCZOS)
    img.save(out_path, "JPEG", quality=90, optimize=True)


def render_html(report: Report, meta: dict[str, Any]) -> str:
    runner_os = meta.get("runner", {}).get("os", "")
    target = meta.get("target", "")
    head = "".join(f"<th>{series_label(s, runner_os, target)}</th>" for s in report.series)
    body = ""
    for sc in report.scenarios():
        cells = ""
        for s in report.series:
            v = report.value(sc, s)
            if v is not None:
                cells += f"<td>{v:.4f}s</td>"
            elif report.is_pending(sc, s):
                cells += '<td class="pending">pending</td>'
            else:
                cells += '<td class="na">n/a</td>'
        body += f"<tr><td>{sc}</td>{cells}</tr>"
    startup_body = "".join(f"<tr><td>{series_label(series, runner_os, target)}</td><td>{seconds:.4f}s</td></tr>" for series, seconds in report.startup_seconds.items())
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><title>reld benchmarks</title>
<style>
body{{background:#0d1117;color:#e6edf3;font:14px system-ui,sans-serif;margin:2rem auto;max-width:960px}}
table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #30363d;padding:.4rem .6rem;text-align:right}}
th:first-child,td:first-child{{text-align:left}}img{{max-width:100%}}a{{color:#58a6ff}}
td.na{{color:#7d8590}}td.pending{{color:#bb8009}}
</style></head><body>
<h1>reld &mdash; link benchmark</h1>
<p>{meta["generated_at"]} &middot; {meta.get("target", "")} &middot; {meta["runner"]["platform"]}</p>
<img src="{IMAGE_NAME}" alt="benchmark chart">
<table><thead><tr><th>Configuration</th>{head}</tr></thead><tbody>{body}</tbody></table>
<h2>Fixed linker startup</h2><p>Reported raw and never subtracted from final-link medians.</p>
<table><thead><tr><th>Series</th><th>Seconds</th></tr></thead><tbody>{startup_body}</tbody></table>
<p><a href="https://github.com/{meta["repository"]}">{meta["repository"]}</a></p>
</body></html>
"""


def write_outputs(report: Report, meta: dict[str, Any], out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)

    # Render first. It is the only step that can reject the data, and appending a history
    # line for a run that never produced a chart corrupts the series permanently.
    render_jpg(report, meta, out_dir / IMAGE_NAME)

    runner_os = meta.get("runner", {}).get("os", "")
    payload = {
        "schema_version": SCHEMA_VERSION,
        "benchmark_id": BENCHMARK_ID,
        "metadata": meta,
        "results": [
            {
                "configuration": r.scenario,
                "series": r.series,
                "seconds": r.seconds,
                # "measured" | "pending" | "na" — pending (unsupported-by-design, documented) is
                # kept distinct from na (failed) so consumers never read a deliberate gap as a
                # regression. See issue #63.
                "status": r.status(),
                "mode": series_mode(r.series, runner_os, meta.get("target", "")),
                "engine": series_engine(r.series, runner_os, meta.get("target", "")),
            }
            for r in report.rows
        ],
        "startup": [
            {
                "series": series,
                "seconds": seconds,
                "status": "measured",
                "mode": series_mode(series, runner_os, meta.get("target", "")),
                "engine": series_engine(series, runner_os, meta.get("target", "")),
            }
            for series, seconds in report.startup_seconds.items()
        ],
    }
    (out_dir / "latest.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")

    history = out_dir / "history.jsonl"
    old_lines = history.read_text(encoding="utf-8").splitlines() if history.exists() else []
    prior = []
    for line in old_lines:
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("schema_version") == SCHEMA_VERSION and entry.get("benchmark_id") == BENCHMARK_ID:
            prior.append(line)
    prior.append(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "benchmark_id": BENCHMARK_ID,
                "ts": meta["generated_at"],
                "sha": meta.get("git_sha", ""),
                "target": meta.get("target", ""),
                "results": payload["results"],
                "startup": payload["startup"],
            },
            separators=(",", ":"),
        )
    )
    history.write_text("\n".join(prior[-HISTORY_MAX_LINES:]) + "\n", encoding="utf-8")

    (out_dir / "index.html").write_text(render_html(report, meta), encoding="utf-8")
    (out_dir / ".nojekyll").write_text("", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="ci.benchmark_stats")
    p.add_argument("--output-dir", type=Path, default=Path("benchmark-stats"))
    p.add_argument("--log-path", type=Path)
    p.add_argument(
        "--metadata-output",
        type=Path,
        help="write native runner metadata beside the raw benchmark log",
    )
    p.add_argument(
        "--metadata-path",
        type=Path,
        help="use metadata captured on the benchmark runner while rendering",
    )
    p.add_argument(
        "--summary-only",
        action="store_true",
        help="parse and print the per-target report without rendering artifacts",
    )
    p.add_argument(
        "--publish-outcome",
        default=os.environ.get("RELD_BENCHMARK_PUBLISH_OUTCOME", "rendered; publication pending"),
        help="outcome shown in the log/Actions summary report",
    )
    p.add_argument(
        "--print-targets",
        action="store_true",
        help="print the canonical target labels, one per line",
    )
    p.add_argument(
        "--check-readme",
        type=Path,
        help="fail unless README's BENCHMARK block matches the canonical target manifest",
    )
    p.add_argument(
        "--write-readme",
        type=Path,
        help="replace README's marker-delimited BENCHMARK block with canonical generated contents",
    )
    p.add_argument(
        "--verify-current-outputs",
        type=Path,
        metavar="ROOT",
        help="verify generated per-target latest.json files match --expected-sha and freshness",
    )
    p.add_argument("--expected-sha", default=os.environ.get("GITHUB_SHA", ""))
    p.add_argument("--max-age-seconds", type=int, default=2 * 60 * 60)
    p.add_argument(
        "--check-remote-freshness",
        action="store_true",
        help="check published per-target latest.json age and measured status",
    )
    p.add_argument(
        "--remote-base-url",
        default=os.environ.get(
            "RELD_BENCHMARK_RAW_BASE_URL",
            "https://raw.githubusercontent.com/zackees/reld/benchmark-stats",
        ),
        help="base URL containing <target>/latest.json (injectable for local tests)",
    )
    src = p.add_mutually_exclusive_group()
    src.add_argument("--run-benchmarks", action="store_true")
    src.add_argument("--input-log", type=Path)
    args = p.parse_args(argv)

    # These maintenance operations deliberately need no benchmark input. Keep them in this
    # module so every workflow consumer draws target identity from one source of truth.
    if args.print_targets:
        print("\n".join(target for target, _ in BENCHMARK_TARGETS))
        return 0
    if args.check_readme:
        if not check_readme_block(args.check_readme):
            sys.stderr.write(f"{args.check_readme}: BENCHMARK block drifted from ci.benchmark_stats manifest\n")
            return 1
        print(f"{args.check_readme}: generated BENCHMARK block matches target manifest")
        return 0
    if args.write_readme:
        try:
            write_readme_block(args.write_readme)
        except ValueError as error:
            sys.stderr.write(f"{args.write_readme}: {error}\n")
            return 1
        print(f"{args.write_readme}: wrote canonical generated BENCHMARK block")
        return 0
    if args.verify_current_outputs:
        errors = verify_current_outputs(args.verify_current_outputs, args.expected_sha, args.max_age_seconds)
        if errors:
            sys.stderr.write("benchmark artifact freshness check failed:\n")
            sys.stderr.write("\n".join(f"- {error}" for error in errors) + "\n")
            return 1
        print(f"benchmark artifacts are current for {args.expected_sha or 'local/unset SHA'} (max age {args.max_age_seconds}s)")
        return 0
    if args.check_remote_freshness:
        errors = verify_remote_freshness(
            args.remote_base_url,
            args.max_age_seconds,
            expected_sha=args.expected_sha,
        )
        lines = [
            "### Published benchmark freshness",
            "",
            "| Field | Value |",
            "|:------|:------|",
            f"| base URL | {args.remote_base_url} |",
            f"| maximum age | {args.max_age_seconds}s |",
            f"| status | {'current' if not errors else 'stale or incomplete'} |",
        ]
        if errors:
            lines.extend(["", "**Freshness check failed:**", *[f"- {error}" for error in errors]])
        summary = "\n".join(lines) + "\n"
        print(summary)
        if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
            with Path(summary_path).open("a", encoding="utf-8") as handle:
                handle.write(summary)
        return 1 if errors else 0

    if not args.run_benchmarks and args.input_log is None:
        p.error("one of --run-benchmarks or --input-log is required")

    if args.run_benchmarks:
        cmd = ["cargo", "run", "--release", "--bin", "reld-bench", "--"]
        proc = subprocess.run(cmd, capture_output=True, text=True)
        text = (proc.stdout or "") + "\n" + (proc.stderr or "")
        if args.log_path:
            args.log_path.parent.mkdir(parents=True, exist_ok=True)
            args.log_path.write_text(text, encoding="utf-8")
        if proc.returncode != 0:
            sys.stderr.write(text)
            return proc.returncode
    else:
        text = args.input_log.read_text(encoding="utf-8")

    report = parse_benchmark_log(text)
    try:
        meta = read_metadata(args.metadata_path, report) if args.metadata_path else collect_metadata(report)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        sys.stderr.write(f"invalid benchmark metadata: {error}\n")
        return 1
    if args.metadata_output:
        args.metadata_output.parent.mkdir(parents=True, exist_ok=True)
        args.metadata_output.write_text(json.dumps(meta, indent=2), encoding="utf-8")
    if args.summary_only:
        summary = render_summary(report, meta, args.publish_outcome)
        print(summary)
        if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
            with Path(summary_path).open("a", encoding="utf-8") as handle:
                handle.write(summary)
        return 0

    write_outputs(report, meta, args.output_dir)
    print(f"wrote {args.output_dir}")
    summary = render_summary(report, meta, args.publish_outcome)
    print(summary)
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
