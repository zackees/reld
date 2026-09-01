"""Render the Linux ELF competitive-linker evidence report without scalarizing it.

The measurement runner owns collection.  This module is intentionally a strict consumer: a
report is rendered only after its fixed contender order, per-contender wall/RSS summaries,
comparison confidence intervals, and raw samples have been validated.  JSON, HTML, PNG, and the
Actions summary are then derived from that one in-memory report.
"""

from __future__ import annotations

import argparse
import html
import json
import math
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
CONTENDER_ORDER = ("bfd", "lld", "mold", "wild", "baseline", "candidate")
METRICS = ("wall_seconds", "peak_rss_kib")
FORBIDDEN_SCORE_KEYS = frozenset({"combined_score", "performance_score", "weighted_score", "score"})
COLORS = {
    "bfd": "#7c3aed",
    "lld": "#0ea5e9",
    "mold": "#f97316",
    "wild": "#22c55e",
    "baseline": "#f59e0b",
    "candidate": "#ec4899",
}


class CompetitionRenderError(ValueError):
    """The measurement report cannot be trusted enough to render."""


@dataclass(frozen=True)
class RenderPaths:
    json: Path
    html: Path
    png: Path
    summary: Path


def _number(value: object, field: str, *, positive: bool = False) -> float:
    if not isinstance(value, int | float) or isinstance(value, bool):
        raise CompetitionRenderError(f"{field} must be a number")
    result = float(value)
    if not math.isfinite(result) or (positive and result <= 0):
        suffix = " a finite positive number" if positive else " finite"
        raise CompetitionRenderError(f"{field} must be{suffix}")
    return result


def _mapping(value: object, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise CompetitionRenderError(f"{field} must be an object")
    return value


def _list(value: object, field: str) -> list[Any]:
    if not isinstance(value, list):
        raise CompetitionRenderError(f"{field} must be an array")
    return value


def _require_exact_keys(value: dict[str, Any], expected: set[str], field: str) -> None:
    missing = sorted(expected - set(value))
    if missing:
        raise CompetitionRenderError(f"{field} is missing: {', '.join(missing)}")


def _reject_scores(value: object, path: str = "report") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key in FORBIDDEN_SCORE_KEYS:
                raise CompetitionRenderError(f"{path}.{key}: scalar combined score is forbidden")
            _reject_scores(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _reject_scores(child, f"{path}[{index}]")


def _validate_summary(summary: object, field: str) -> None:
    summary = _mapping(summary, field)
    _require_exact_keys(
        summary,
        {"median", "median_absolute_deviation", "min", "max", "bootstrap_95_ci"},
        field,
    )
    median = _number(summary["median"], f"{field}.median", positive=True)
    mad = _number(summary["median_absolute_deviation"], f"{field}.median_absolute_deviation")
    minimum = _number(summary["min"], f"{field}.min", positive=True)
    maximum = _number(summary["max"], f"{field}.max", positive=True)
    ci = _list(summary["bootstrap_95_ci"], f"{field}.bootstrap_95_ci")
    if len(ci) != 2:
        raise CompetitionRenderError(f"{field}.bootstrap_95_ci must contain exactly two values")
    low = _number(ci[0], f"{field}.bootstrap_95_ci[0]", positive=True)
    high = _number(ci[1], f"{field}.bootstrap_95_ci[1]", positive=True)
    if mad < 0 or minimum > median or median > maximum or low > high:
        raise CompetitionRenderError(f"{field} has inconsistent summary bounds")


def validate_report(report: object) -> None:
    """Validate the schema required for an aligned two-metric evidence surface."""
    report = _mapping(report, "report")
    _reject_scores(report)
    if report.get("schema_version") != SCHEMA_VERSION:
        raise CompetitionRenderError(f"schema_version must be {SCHEMA_VERSION}")
    workload = _mapping(report.get("workload"), "workload")
    workload_id = workload.get("id")
    if not isinstance(workload_id, str) or not workload_id:
        raise CompetitionRenderError("workload.id must be a non-empty string")

    order = _list(report.get("contender_order"), "contender_order")
    if tuple(order) != CONTENDER_ORDER:
        raise CompetitionRenderError(f"contender_order must be exactly {list(CONTENDER_ORDER)!r}")
    contenders = _mapping(report.get("contenders"), "contenders")
    if set(contenders) != set(CONTENDER_ORDER):
        raise CompetitionRenderError("contenders must contain exactly the fixed contender order")
    for contender in CONTENDER_ORDER:
        detail = _mapping(contenders[contender], f"contenders.{contender}")
        label = detail.get("label")
        if not isinstance(label, str) or not label:
            raise CompetitionRenderError(f"contenders.{contender}.label must be a non-empty string")
        summaries = _mapping(detail.get("summaries"), f"contenders.{contender}.summaries")
        if set(summaries) != set(METRICS):
            raise CompetitionRenderError(f"contenders.{contender}.summaries must contain wall_seconds and peak_rss_kib")
        for metric in METRICS:
            _validate_summary(summaries[metric], f"contenders.{contender}.summaries.{metric}")

    comparisons = _list(report.get("comparisons"), "comparisons")
    if not comparisons:
        raise CompetitionRenderError("comparisons must not be empty")
    for index, comparison in enumerate(comparisons):
        detail = _mapping(comparison, f"comparisons[{index}]")
        reference = detail.get("reference")
        candidate = detail.get("candidate")
        if reference not in CONTENDER_ORDER or candidate not in CONTENDER_ORDER or reference == candidate:
            raise CompetitionRenderError(f"comparisons[{index}] must name two distinct fixed contenders")
        metrics = _mapping(detail.get("metrics"), f"comparisons[{index}].metrics")
        if set(metrics) != set(METRICS):
            raise CompetitionRenderError(f"comparisons[{index}].metrics must contain wall_seconds and peak_rss_kib")
        for metric in METRICS:
            item = _mapping(metrics[metric], f"comparisons[{index}].metrics.{metric}")
            ci = _list(item.get("bootstrap_95_ci"), f"comparisons[{index}].metrics.{metric}.bootstrap_95_ci")
            if len(ci) != 2:
                raise CompetitionRenderError(f"comparisons[{index}].metrics.{metric}.bootstrap_95_ci must contain exactly two values")
            if _number(ci[0], f"comparisons[{index}].metrics.{metric}.bootstrap_95_ci[0]") > _number(ci[1], f"comparisons[{index}].metrics.{metric}.bootstrap_95_ci[1]"):
                raise CompetitionRenderError(f"comparisons[{index}].metrics.{metric}.bootstrap_95_ci has reversed bounds")

    samples = _list(report.get("raw_samples"), "raw_samples")
    if not samples:
        raise CompetitionRenderError("raw_samples must not be empty")
    seen = {name: 0 for name in CONTENDER_ORDER}
    rounds: dict[int, list[tuple[str, int]]] = {}
    for index, sample in enumerate(samples):
        detail = _mapping(sample, f"raw_samples[{index}]")
        _require_exact_keys(
            detail,
            {"round", "position", "contender", "wall_seconds", "peak_rss_kib", "output_sha256", "metric_backend"},
            f"raw_samples[{index}]",
        )
        contender = detail["contender"]
        if contender not in seen:
            raise CompetitionRenderError(f"raw_samples[{index}].contender is not in contender_order")
        if not isinstance(detail["round"], int) or detail["round"] < 0:
            raise CompetitionRenderError(f"raw_samples[{index}].round must be a non-negative integer")
        if not isinstance(detail["position"], int) or detail["position"] < 0:
            raise CompetitionRenderError(f"raw_samples[{index}].position must be a non-negative integer")
        _number(detail["wall_seconds"], f"raw_samples[{index}].wall_seconds", positive=True)
        _number(detail["peak_rss_kib"], f"raw_samples[{index}].peak_rss_kib", positive=True)
        digest = detail["output_sha256"]
        if not isinstance(digest, str) or len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
            raise CompetitionRenderError(f"raw_samples[{index}].output_sha256 must be a lowercase SHA-256 digest")
        if not isinstance(detail["metric_backend"], str) or not detail["metric_backend"]:
            raise CompetitionRenderError(f"raw_samples[{index}].metric_backend must be a non-empty string")
        seen[contender] += 1
        rounds.setdefault(detail["round"], []).append((contender, detail["position"]))
    missing = [name for name, count in seen.items() if not count]
    if missing:
        raise CompetitionRenderError(f"raw_samples omitted contender(s): {', '.join(missing)}")
    for round_index, entries in rounds.items():
        contenders_in_round = [contender for contender, _ in entries]
        positions = [position for _, position in entries]
        if set(contenders_in_round) != set(CONTENDER_ORDER) or len(entries) != len(CONTENDER_ORDER):
            raise CompetitionRenderError(f"raw_samples round {round_index} must contain every contender exactly once")
        if set(positions) != set(range(len(CONTENDER_ORDER))):
            raise CompetitionRenderError(f"raw_samples round {round_index} must use every contender position exactly once")


def _font(size: int):
    from PIL import ImageFont

    for candidate in (
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/Library/Fonts/Arial.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ):
        try:
            return ImageFont.truetype(candidate, size=size)
        except OSError:
            pass
    return ImageFont.load_default()


def _hex_rgb(value: str) -> tuple[int, int, int]:
    return tuple(int(value[index : index + 2], 16) for index in (1, 3, 5))


def _format_metric(metric: str, value: float) -> str:
    if metric == "wall_seconds":
        return f"{value:.3f} s"
    return f"{value / 1024:.3f} MiB"


def _summary_values(report: dict[str, Any], metric: str) -> list[dict[str, Any]]:
    values = []
    for contender in CONTENDER_ORDER:
        detail = report["contenders"][contender]
        values.append({"name": contender, "label": detail["label"], "summary": detail["summaries"][metric]})
    return values


def _rendered_values(report: dict[str, Any]) -> dict[str, dict[str, dict[str, object]]]:
    """Return the absolute medians/CIs printed in both visual surfaces.

    Keeping this compact representation in PNG metadata lets parity tests verify the binary
    chart without trying to OCR its labels. It contains no derived score or normalized value.
    """
    return {
        contender: {
            metric: {
                "median": report["contenders"][contender]["summaries"][metric]["median"],
                "bootstrap_95_ci": report["contenders"][contender]["summaries"][metric]["bootstrap_95_ci"],
            }
            for metric in METRICS
        }
        for contender in CONTENDER_ORDER
    }


def render_png(report: dict[str, Any], path: Path) -> None:
    """Render wall time and RSS as aligned panels with independent zero-based scales."""
    from PIL import Image, ImageDraw, PngImagePlugin

    validate_report(report)
    width, height, scale = 1800, 920, 2
    image = Image.new("RGB", (width * scale, height * scale), "#0d1117")
    draw = ImageDraw.Draw(image)
    title_font = _font(30 * scale)
    panel_font = _font(22 * scale)
    label_font = _font(15 * scale)
    small_font = _font(12 * scale)
    fg, muted, grid = "#e6edf3", "#9da7b3", "#30363d"
    draw.text((48 * scale, 30 * scale), "Linux ELF competitive linker evidence", font=title_font, fill=fg)
    draw.text(
        (48 * scale, 74 * scale),
        f"Workload: {report['workload']['id']}  |  Independent zero-based scales; no combined score",
        font=label_font,
        fill=muted,
    )

    panels = (("wall_seconds", "Wall link time (seconds)"), ("peak_rss_kib", "Peak process-tree RSS (MiB)"))
    panel_width, left_margin, top, chart_height = 810, 60, 165, 540
    bar_width = 78
    for panel_index, (metric, title) in enumerate(panels):
        x0 = left_margin + panel_index * 885
        y_axis = top + chart_height
        values = _summary_values(report, metric)
        raw_max = max(float(item["summary"]["bootstrap_95_ci"][1]) for item in values)
        if metric == "peak_rss_kib":
            raw_max /= 1024
        axis_max = max(raw_max * 1.20, 0.001)
        draw.text((x0 * scale, 120 * scale), title, font=panel_font, fill=fg)
        draw.rectangle((x0 * scale, top * scale, (x0 + panel_width) * scale, y_axis * scale), outline=grid, width=2 * scale)
        for tick in range(6):
            value = axis_max * tick / 5
            y = y_axis - chart_height * tick / 5
            draw.line((x0 * scale, int(y * scale), (x0 + panel_width) * scale, int(y * scale)), fill=grid, width=1 * scale)
            unit = "s" if metric == "wall_seconds" else "MiB"
            draw.text((x0 * scale, int(y * scale) - 13 * scale), f"{value:.2f} {unit}", font=small_font, fill=muted)
        gap = panel_width / len(values)
        for index, item in enumerate(values):
            summary = item["summary"]
            median = float(summary["median"])
            low, high = (float(value) for value in summary["bootstrap_95_ci"])
            if metric == "peak_rss_kib":
                median, low, high = median / 1024, low / 1024, high / 1024
            center = x0 + gap * (index + 0.5)
            bar_top = y_axis - chart_height * median / axis_max
            color = _hex_rgb(COLORS[item["name"]])
            draw.rectangle(
                (int((center - bar_width / 2) * scale), int(bar_top * scale), int((center + bar_width / 2) * scale), y_axis * scale),
                fill=color,
            )
            low_y = y_axis - chart_height * low / axis_max
            high_y = y_axis - chart_height * high / axis_max
            draw.line((int(center * scale), int(low_y * scale), int(center * scale), int(high_y * scale)), fill=fg, width=2 * scale)
            draw.line((int((center - 10) * scale), int(low_y * scale), int((center + 10) * scale), int(low_y * scale)), fill=fg, width=2 * scale)
            draw.line((int((center - 10) * scale), int(high_y * scale), int((center + 10) * scale), int(high_y * scale)), fill=fg, width=2 * scale)
            value_label = _format_metric(metric, float(summary["median"]))
            ci_label = f"95% CI [{_format_metric(metric, float(summary['bootstrap_95_ci'][0]))}, {_format_metric(metric, float(summary['bootstrap_95_ci'][1]))}]"
            text_y = max(top + 3, high_y - 39)
            draw.text((int((center - gap * 0.43) * scale), int(text_y * scale)), value_label, font=small_font, fill=fg)
            draw.text((int((center - gap * 0.43) * scale), int((text_y + 16) * scale)), ci_label, font=small_font, fill=muted)
            label = item["label"]
            box = draw.textbbox((0, 0), label, font=label_font)
            draw.text((int((center - (box[2] - box[0]) / scale / 2) * scale), int((y_axis + 12) * scale)), label, font=label_font, fill=fg)

    footer_y = 790
    draw.text((48 * scale, footer_y * scale), "Bars = median. Whiskers = per-contender bootstrap 95% CI. Metrics are intentionally not scalarized.", font=label_font, fill=muted)
    path.parent.mkdir(parents=True, exist_ok=True)
    metadata = PngImagePlugin.PngInfo()
    metadata.add_text("workload", str(report["workload"]["id"]))
    metadata.add_text("contender_order", ",".join(CONTENDER_ORDER))
    metadata.add_text("metrics", "wall_seconds,peak_rss_kib")
    metadata.add_text("rendered_values", json.dumps(_rendered_values(report), sort_keys=True, separators=(",", ":")))
    image.resize((width, height), Image.Resampling.LANCZOS).save(path, "PNG", pnginfo=metadata)


def _html_table(report: dict[str, Any]) -> str:
    rows = []
    for contender in CONTENDER_ORDER:
        detail = report["contenders"][contender]
        wall = detail["summaries"]["wall_seconds"]
        rss = detail["summaries"]["peak_rss_kib"]
        rows.append(
            "<tr>"
            f"<th scope=\"row\"><span class=\"swatch\" style=\"background:{COLORS[contender]}\"></span>{html.escape(detail['label'])}</th>"
            f"<td>{_format_metric('wall_seconds', float(wall['median']))}</td>"
            f"<td>95% CI [{_format_metric('wall_seconds', float(wall['bootstrap_95_ci'][0]))}, {_format_metric('wall_seconds', float(wall['bootstrap_95_ci'][1]))}]</td>"
            f"<td>{_format_metric('peak_rss_kib', float(rss['median']))}</td>"
            f"<td>95% CI [{_format_metric('peak_rss_kib', float(rss['bootstrap_95_ci'][0]))}, {_format_metric('peak_rss_kib', float(rss['bootstrap_95_ci'][1]))}]</td>"
            "</tr>"
        )
    return "\n".join(rows)


def render_html(report: dict[str, Any]) -> str:
    validate_report(report)
    return f"""<!doctype html>
<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Linux linker competition</title>
<style>
body{{background:#0d1117;color:#e6edf3;font:16px system-ui,sans-serif;margin:2rem auto;max-width:1200px}}
table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #30363d;padding:.55rem;text-align:right}}th{{text-align:left}}
.swatch{{display:inline-block;width:.8rem;height:.8rem;border-radius:50%;margin-right:.4rem}}figure{{margin:1.5rem 0}}img{{max-width:100%;height:auto}}
</style></head><body>
<h1>Linux ELF competitive linker evidence</h1>
<p>Workload: <code>{html.escape(report['workload']['id'])}</code>. Wall link time and peak process-tree RSS use independent zero-based scales. No scalar combined score is produced.</p>
<figure aria-label=\"Competitive linker measurements\"><img src=\"competition.png\" alt=\"Aligned panels for wall link time in seconds and peak process-tree RSS in MiB, in the same fixed contender order.\"><figcaption>Bars show medians; whiskers and labels show per-contender 95% confidence intervals.</figcaption></figure>
<h2>Accessible data table</h2>
<table><thead><tr><th>Linker</th><th>Wall link time (seconds)</th><th>Wall 95% CI</th><th>Peak process-tree RSS (MiB)</th><th>RSS 95% CI</th></tr></thead>
<tbody>{_html_table(report)}</tbody></table>
</body></html>
"""


def _format_comparison(comparison: dict[str, Any], report: dict[str, Any]) -> str:
    reference = report["contenders"][comparison["reference"]]["label"]
    candidate = report["contenders"][comparison["candidate"]]["label"]
    wall = comparison["metrics"]["wall_seconds"]["bootstrap_95_ci"]
    rss = comparison["metrics"]["peak_rss_kib"]["bootstrap_95_ci"]
    wall_text = f"wall: {wall[0]:+.1%} to {wall[1]:+.1%}"
    rss_text = f"RSS: {rss[0]:+.1%} to {rss[1]:+.1%}"
    verdict = "aggregate better" if wall[0] > 0 and rss[0] > 0 else "no aggregate better claim"
    return f"- {html.escape(candidate)} vs {html.escape(reference)} — {wall_text}; {rss_text}; **{verdict}**."


def render_summary(report: dict[str, Any]) -> str:
    validate_report(report)
    lines = [
        "### Linux ELF competitive linker evidence",
        "",
        f"Workload: `{report['workload']['id']}`. Wall time and peak RSS are co-primary; No scalar combined score.",
        "",
        "| Linker | Wall link time (s) | Wall 95% CI | Peak RSS (MiB) | RSS 95% CI |",
        "|:--|--:|:--|--:|:--|",
    ]
    for contender in CONTENDER_ORDER:
        detail = report["contenders"][contender]
        wall = detail["summaries"]["wall_seconds"]
        rss = detail["summaries"]["peak_rss_kib"]
        lines.append(
            f"| {detail['label']} | {_format_metric('wall_seconds', float(wall['median']))} | "
            f"[{_format_metric('wall_seconds', float(wall['bootstrap_95_ci'][0]))}, {_format_metric('wall_seconds', float(wall['bootstrap_95_ci'][1]))}] | "
            f"{_format_metric('peak_rss_kib', float(rss['median']))} | "
            f"[{_format_metric('peak_rss_kib', float(rss['bootstrap_95_ci'][0]))}, {_format_metric('peak_rss_kib', float(rss['bootstrap_95_ci'][1]))}] |"
        )
    lines.extend(["", "#### Paired comparison confidence intervals", "", *[_format_comparison(item, report) for item in report["comparisons"]]])
    return "\n".join(lines) + "\n"


def render_report(report: object, output_dir: Path) -> RenderPaths:
    validate_report(report)
    assert isinstance(report, dict)  # Narrowed by validate_report.
    output_dir.mkdir(parents=True, exist_ok=True)
    paths = RenderPaths(
        json=output_dir / "competition-report.json",
        html=output_dir / "competition.html",
        png=output_dir / "competition.png",
        summary=output_dir / "summary.md",
    )
    paths.json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    render_png(report, paths.png)
    paths.html.write_text(render_html(report), encoding="utf-8")
    paths.summary.write_text(render_summary(report), encoding="utf-8")
    return paths


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--summary-output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        report = json.loads(args.report.read_text(encoding="utf-8"))
        paths = render_report(report, args.output_dir)
        args.summary_output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(paths.summary, args.summary_output)
    except (CompetitionRenderError, OSError, json.JSONDecodeError) as error:
        parser.exit(1, f"linux linker competition render failed: {error}\n")
    print(f"rendered competitive evidence: {paths.png}, {paths.html}, {paths.summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
