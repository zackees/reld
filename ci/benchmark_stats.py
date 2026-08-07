"""Parse reld-bench markdown output and render the published benchmark chart.

Contract with the benchmark runner is deliberately tiny: print a markdown table under a
heading that starts with ``## Link Benchmark:``, whose first column is ``Scenario`` and whose
remaining columns are linker names. Cells are seconds, or ``n/a`` when a linker was not
available on the runner.

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
import json
import os
import platform
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 3
HISTORY_MAX_LINES = 1000
IMAGE_NAME = "benchmark-link.jpg"
HEADING_PREFIX = "## Link Benchmark:"

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
    """Scrape the first ``## Link Benchmark:`` table out of a log."""
    report = Report()
    in_table = False
    header: list[str] = []

    # A UTF-8 BOM ahead of the first heading otherwise makes startswith() miss and the whole
    # log parse silently to zero rows.
    text = text.lstrip("﻿")

    for raw in text.splitlines():
        line = raw.strip().lstrip("﻿")

        if line.startswith(HEADING_PREFIX):
            report.label = line[len(HEADING_PREFIX):].strip()
            in_table = True
            header = []
            continue

        if not in_table:
            continue

        if not line.startswith("|"):
            # A blank line inside the table block is tolerated; anything else ends it.
            if line == "" or line.startswith("<!--"):
                continue
            in_table = False
            continue

        cells = [_clean(c) for c in line.strip("|").split("|")]
        if not header:
            header = cells
            report.series = [c for c in cells[1:] if c]
            continue
        if set("".join(cells)) <= set("-: "):
            continue  # the |---|---| separator row

        scenario = cells[0]
        if not scenario:
            continue
        for idx, series in enumerate(report.series, start=1):
            value, pending = _classify(cells[idx]) if idx < len(cells) else (None, False)
            report.rows.append(
                Row(scenario=scenario, series=series, seconds=value, pending=pending)
            )

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
        "raw_image_url": f"{raw_base}/{IMAGE_NAME}",
        "pages_url": os.environ.get("RELD_BENCHMARK_PAGES_URL"),
    }


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
    footer_h = 34
    height = header_h + len(scenarios) * group_h + footer_h

    img = Image.new("RGB", (WIDTH * SCALE, height * SCALE), BG)
    d = ImageDraw.Draw(img)
    f_title = _font(19 * SCALE)
    f_meta = _font(11 * SCALE)
    f_label = _font(12 * SCALE)

    d.rectangle([0, 0, WIDTH * SCALE, header_h * SCALE], fill=PANEL)
    d.text((20 * SCALE, 16 * SCALE), "reld - link benchmark", font=f_title, fill=FG)
    sha = (meta.get("git_sha") or "")[:12]
    sub = f"{meta['generated_at']}  |  {meta.get('target','')}  |  {meta['runner']['platform']}"
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
            d.text((34 * SCALE, (y + 5) * SCALE), s, font=f_meta, fill=MUTED)

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

    d.text(
        (20 * SCALE, (height - 24) * SCALE),
        "lower is better  |  median of N trials, link step only  |  "
        "n/a = linker failed/unavailable  |  pending = unsupported-by-design (documented)",
        font=f_meta,
        fill=MUTED,
    )

    img = img.resize((WIDTH, height), Image.Resampling.LANCZOS)
    img.save(out_path, "JPEG", quality=90, optimize=True)


def render_html(report: Report, meta: dict[str, Any]) -> str:
    head = "".join(f"<th>{s}</th>" for s in report.series)
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
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><title>reld benchmarks</title>
<style>
body{{background:#0d1117;color:#e6edf3;font:14px system-ui,sans-serif;margin:2rem auto;max-width:960px}}
table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #30363d;padding:.4rem .6rem;text-align:right}}
th:first-child,td:first-child{{text-align:left}}img{{max-width:100%}}a{{color:#58a6ff}}
td.na{{color:#7d8590}}td.pending{{color:#bb8009}}
</style></head><body>
<h1>reld &mdash; link benchmark</h1>
<p>{meta['generated_at']} &middot; {meta.get('target','')} &middot; {meta['runner']['platform']}</p>
<img src="{IMAGE_NAME}" alt="benchmark chart">
<table><thead><tr><th>Scenario</th>{head}</tr></thead><tbody>{body}</tbody></table>
<p><a href="https://github.com/{meta['repository']}">{meta['repository']}</a></p>
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
        "metadata": meta,
        "results": [
            {
                "scenario": r.scenario,
                "series": r.series,
                "seconds": r.seconds,
                # "measured" | "pending" | "na" — pending (unsupported-by-design, documented) is
                # kept distinct from na (failed) so consumers never read a deliberate gap as a
                # regression. See issue #63.
                "status": r.status(),
                "mode": series_mode(r.series, runner_os, meta.get("target", "")),
            }
            for r in report.rows
        ],
    }
    (out_dir / "latest.json").write_text(json.dumps(payload, indent=2), encoding="utf-8")

    history = out_dir / "history.jsonl"
    prior = history.read_text(encoding="utf-8").splitlines() if history.exists() else []
    prior.append(
        json.dumps(
            {
                "ts": meta["generated_at"],
                "sha": meta.get("git_sha", ""),
                "target": meta.get("target", ""),
                "results": payload["results"],
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
    src = p.add_mutually_exclusive_group(required=True)
    src.add_argument("--run-benchmarks", action="store_true")
    src.add_argument("--input-log", type=Path)
    args = p.parse_args(argv)

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
    write_outputs(report, collect_metadata(report), args.output_dir)
    print(f"wrote {args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
