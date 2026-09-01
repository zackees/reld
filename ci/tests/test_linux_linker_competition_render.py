"""RED-to-GREEN contract tests for the Linux linker competition renderer."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from PIL import Image

from ci.linux_linker_competition_render import (
    CONTENDER_ORDER,
    CompetitionRenderError,
    main,
    render_report,
    render_summary,
    validate_report,
)


def _summary(median: float) -> dict[str, object]:
    return {
        "median": median,
        "median_absolute_deviation": median / 100,
        "min": median * 0.95,
        "max": median * 1.05,
        "bootstrap_95_ci": [median * 0.90, median * 1.10],
    }


def _report() -> dict[str, object]:
    medians = {
        "bfd": (3.57, 983.0 * 1024),
        "lld": (0.59, 357.0 * 1024),
        "mold": (0.27, 292.0 * 1024),
        "wild": (0.13, 239.0 * 1024),
        "baseline": (0.15, 241.0 * 1024),
        "candidate": (0.12, 231.0 * 1024),
    }

    contenders = {
        name: {
            "label": label,
            "summaries": {
                "wall_seconds": _summary(wall),
                "peak_rss_kib": _summary(rss),
            },
        }
        for name, label, (wall, rss) in (
            ("bfd", "GNU bfd", medians["bfd"]),
            ("lld", "LLD", medians["lld"]),
            ("mold", "mold", medians["mold"]),
            ("wild", "Wild", medians["wild"]),
            ("baseline", "reld baseline", medians["baseline"]),
            ("candidate", "reld candidate", medians["candidate"]),
        )
    }
    samples = []
    for round_index in range(10):
        for position, contender in enumerate(CONTENDER_ORDER):
            wall, rss = medians[contender]
            samples.append(
                {
                    "round": round_index,
                    "position": position,
                    "contender": contender,
                    "wall_seconds": wall + round_index / 10_000,
                    "peak_rss_kib": rss + round_index,
                    "output_sha256": "a" * 64,
                    "metric_backend": "cgroup-v2-process-tree-rss",
                }
            )
    return {
        "schema_version": 1,
        "workload": {"id": "llvmorg-22.1.8-clang-final-link"},
        "contender_order": list(CONTENDER_ORDER),
        "contenders": contenders,
        "comparisons": [
            {
                "reference": "baseline",
                "candidate": "candidate",
                "metrics": {
                    "wall_seconds": {"bootstrap_95_ci": [0.01, 0.20]},
                    "peak_rss_kib": {"bootstrap_95_ci": [0.01, 0.10]},
                },
            }
        ],
        "raw_samples": samples,
    }


def _realistic_issue_103_report() -> dict[str, object]:
    """A measurement-shaped report with optional provenance and cgroup diagnostics."""
    report = _report()
    report.update(
        {
            "status": "passed",
            "target": "x86_64-unknown-linux-gnu",
            "metric_backend": "cgroup-v2-process-tree-rss",
            "provenance": {
                "runner": {"image": "ubuntu-24.04", "kernel": "6.8.0", "cpu_model": "example CPU"},
                "corpus_lock_sha256": "b" * 64,
                "baseline_source_sha": "c" * 40,
                "candidate_source_sha": "d" * 40,
            },
            "identity": {"status": "passed", "first_differing_offset": None},
        }
    )
    for contender in CONTENDER_ORDER:
        detail = report["contenders"][contender]
        detail["binary"] = {"sha256": "e" * 64, "version": f"{contender} version"}
        detail["artifact_identity"] = {"sha256": "a" * 64, "self_deterministic": True}
        detail["diagnostics"] = {"cgroup_memory_peak_bytes": 1024 * 1024, "cpu_usage_usec": 1_000}
        for metric in ("wall_seconds", "peak_rss_kib"):
            detail["summaries"][metric]["sample_count"] = 10
            detail["summaries"][metric]["unit"] = "seconds" if metric == "wall_seconds" else "KiB"
    report["comparisons"][0].update(
        {"method": "paired bootstrap median ratio", "iterations": 20_000, "seed": 103}
    )
    for sample in report["raw_samples"]:
        sample.update(
            {
                "order": list(CONTENDER_ORDER),
                "cgroup_memory_peak_bytes": 1_024 * 1_024,
                "cpu_usage_usec": 100_000,
                "cgroup_path": "/sys/fs/cgroup/reld/example",
                "identity_sha256": "a" * 64,
            }
        )
    return report


def test_report_contract_is_fixed_and_accepts_complete_evidence():
    report = _report()

    validate_report(report)

    assert CONTENDER_ORDER == ("bfd", "lld", "mold", "wild", "baseline", "candidate")


@pytest.mark.parametrize(
    ("mutate", "match"),
    [
        (lambda report: report.__setitem__("contender_order", list(reversed(CONTENDER_ORDER))), "contender_order"),
        (lambda report: report["contenders"]["lld"]["summaries"].pop("peak_rss_kib"), "peak_rss_kib"),
        (lambda report: report.__setitem__("combined_score", 1.0), "combined score"),
        (lambda report: report["contenders"]["mold"]["summaries"]["wall_seconds"].__setitem__("bootstrap_95_ci", [0.1]), "bootstrap_95_ci"),
        (lambda report: report["raw_samples"].pop(), "raw_samples"),
    ],
)
def test_report_contract_rejects_red_cases(mutate, match):
    report = _report()
    mutate(report)

    with pytest.raises(CompetitionRenderError, match=match):
        validate_report(report)


def test_render_surfaces_have_aligned_wall_and_rss_values(tmp_path: Path):
    report = _report()

    paths = render_report(report, tmp_path)
    html = paths.html.read_text(encoding="utf-8")
    summary = paths.summary.read_text(encoding="utf-8")

    assert paths.json.is_file()
    assert paths.html.is_file()
    assert paths.png.is_file()
    assert paths.summary.is_file()
    assert "Wall link time (seconds)" in html
    assert "Peak process-tree RSS (MiB)" in html
    assert 'aria-label="Competitive linker measurements"' in html
    assert "GNU bfd" in html
    assert "3.570 s" in html
    assert "983.000 MiB" in html
    assert "95% CI" in html
    assert "Wall link time (s)" in summary
    assert "Peak RSS (MiB)" in summary
    assert "No scalar combined score" in summary
    assert json.loads(paths.json.read_text(encoding="utf-8"))["contender_order"] == list(CONTENDER_ORDER)


def test_realistic_measurement_schema_and_every_surface_stay_in_parity(tmp_path: Path):
    report = _realistic_issue_103_report()

    validate_report(report)
    paths = render_report(report, tmp_path)
    copied = json.loads(paths.json.read_text(encoding="utf-8"))
    html = paths.html.read_text(encoding="utf-8")
    markdown = paths.summary.read_text(encoding="utf-8")
    with Image.open(paths.png) as image:
        png_values = json.loads(image.text["rendered_values"])
        assert image.text["contender_order"].split(",") == list(CONTENDER_ORDER)
        assert image.text["metrics"] == "wall_seconds,peak_rss_kib"

    # The verbatim copy retains raw samples and optional cgroup/provenance diagnostics.
    assert copied == report
    assert copied["raw_samples"][0]["metric_backend"] == "cgroup-v2-process-tree-rss"
    assert copied["raw_samples"][0]["cgroup_memory_peak_bytes"] == 1_024 * 1_024
    assert copied["contenders"]["candidate"]["diagnostics"]["cpu_usage_usec"] == 1_000

    # HTML, Markdown, and PNG metadata all use the same fixed order and absolute values/CIs.
    for contender in CONTENDER_ORDER:
        detail = report["contenders"][contender]
        wall = detail["summaries"]["wall_seconds"]
        rss = detail["summaries"]["peak_rss_kib"]
        assert detail["label"] in html
        assert detail["label"] in markdown
        assert f"{wall['median']:.3f} s" in html
        assert f"{wall['median']:.3f} s" in markdown
        assert f"{rss['median'] / 1024:.3f} MiB" in html
        assert f"{rss['median'] / 1024:.3f} MiB" in markdown
        assert png_values[contender]["wall_seconds"]["median"] == wall["median"]
        assert png_values[contender]["wall_seconds"]["bootstrap_95_ci"] == wall["bootstrap_95_ci"]
        assert png_values[contender]["peak_rss_kib"]["median"] == rss["median"]
        assert png_values[contender]["peak_rss_kib"]["bootstrap_95_ci"] == rss["bootstrap_95_ci"]


def test_summary_rejects_wall_only_or_rss_only_claims():
    report = _report()
    report["comparisons"][0]["metrics"]["peak_rss_kib"]["bootstrap_95_ci"] = [-0.10, 0.10]

    summary = render_summary(report)

    assert "no aggregate better claim" in summary
    assert "wall: +1.0% to +20.0%" in summary
    assert "RSS: -10.0% to +10.0%" in summary


def test_cli_writes_summary_to_explicit_path(tmp_path: Path):
    report_path = tmp_path / "report.json"
    output_dir = tmp_path / "rendered"
    summary_path = tmp_path / "job-summary.md"
    report_path.write_text(json.dumps(_report()), encoding="utf-8")

    assert main(["--report", str(report_path), "--output-dir", str(output_dir), "--summary-output", str(summary_path)]) == 0

    assert summary_path.read_text(encoding="utf-8") == (output_dir / "summary.md").read_text(encoding="utf-8")
