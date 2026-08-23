"""Parser and renderer tests.

These run off a hardcoded log so chart layout can be iterated on without a real benchmark.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from ci.benchmark_stats import (  # noqa: E402
    BENCHMARK_TARGETS,
    CANONICAL_SCENARIOS,
    EXPECTED_SERIES,
    check_readme_block,
    collect_metadata,
    parse_benchmark_log,
    render_readme_block,
    render_summary,
    series_engine,
    series_label,
    series_mode,
    verify_current_outputs,
    verify_remote_freshness,
    write_readme_block,
    write_outputs,
)

SAMPLE_LOG = """
   Compiling reld-testkit v0.0.0
    Finished `release` profile

## Link Benchmark: x86_64-linux

| Scenario | bfd | lld | mold | wild | reld |
|:---------|----:|----:|-----:|-----:|----:|
| small (16 units) | 0.0210 | 0.0081 | 0.0062 | 0.0044 | n/a |
| medium (128 units) | 0.1400 | 0.0390 | 0.0221 | 0.0155 | n/a |
| large (512 units) | 0.6120 | 0.1602 | 0.0904 | 0.0631 | n/a |

<!-- linker wild not available on this runner -->
"""


def test_parses_label_and_series():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.label == "x86_64-linux"
    assert r.series == ["bfd", "lld", "mold", "wild", "reld"]


def test_parses_scenarios_in_order():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.scenarios() == [
        "small (16 units)",
        "medium (128 units)",
        "large (512 units)",
    ]


def test_parses_values_and_na():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.value("small (16 units)", "bfd") == pytest.approx(0.0210)
    assert r.value("large (512 units)", "wild") == pytest.approx(0.0631)
    # The unimplemented column must survive as None, not as 0.0 — a zero would render as
    # an infinitely fast linker, which is exactly the kind of accidental lie this guards.
    assert r.value("small (16 units)", "reld") is None


def test_row_count():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert len(r.rows) == 3 * 5


def test_ignores_noise_outside_table():
    r = parse_benchmark_log("garbage\n## Not A Benchmark:\n| a | b |\n" + SAMPLE_LOG)
    assert r.label == "x86_64-linux"
    assert len(r.rows) == 15


def test_empty_log_yields_nothing():
    r = parse_benchmark_log("no tables here")
    assert r.rows == []
    assert r.series == []


def test_write_outputs_produces_all_artifacts(tmp_path):
    r = parse_benchmark_log(SAMPLE_LOG)
    write_outputs(r, collect_metadata(r), tmp_path)
    for name in ("benchmark-link.jpg", "latest.json", "history.jsonl", "index.html", ".nojekyll"):
        assert (tmp_path / name).exists(), name

    payload = json.loads((tmp_path / "latest.json").read_text())
    assert payload["schema_version"] == 4
    assert len(payload["results"]) == 15
    for entry in payload["results"]:
        assert "mode" in entry
        assert "engine" in entry
        assert "status" in entry
        if entry["series"] in ("bfd", "lld"):
            assert entry["mode"] == "reference"
            assert entry["status"] == "measured"
        if entry["series"] == "reld":
            # SAMPLE_LOG has reld as a bare n/a (not a pending marker), so it is a failure.
            assert entry["status"] == "na"


def test_series_mode():
    assert series_mode("reld", "Linux") == "native"
    assert series_mode("reld", "Windows") == "bridge"
    assert series_mode("reld", "Darwin") == "bridge"
    assert series_mode("bfd", "Linux") == "reference"
    assert series_mode("lld", "Windows") == "reference"


def test_series_mode_uses_target_when_rendering_on_aggregation_runner():
    assert series_mode("reld", "Linux", "x86_64-pc-windows-msvc") == "bridge"
    assert series_mode("reld", "Linux", "aarch64-apple-darwin") == "bridge"
    assert series_mode("reld", "Linux", "x86_64-linux") == "native"


def test_reld_engine_and_labels_make_bridge_measurements_unambiguous():
    assert series_engine("reld", "Linux", "x86_64-linux") == "reld"
    assert series_engine("reld", "Linux", "x86_64-pc-windows-msvc") == "lld-link"
    assert series_engine("reld", "Linux", "aarch64-apple-darwin") == "ld64.lld"
    assert series_label("reld", "Linux", "aarch64-apple-darwin") == "reld (bridge/ld64.lld)"
    assert series_label("lld", "Linux", "x86_64-linux") == "lld"


def test_summary_includes_timings_metadata_and_publish_state(monkeypatch):
    monkeypatch.setenv("GITHUB_SHA", "abc123")
    report = parse_benchmark_log(SAMPLE_LOG)
    summary = render_summary(report, collect_metadata(report), "uploaded")
    assert "source SHA | abc123" in summary
    assert "publish outcome | uploaded" in summary
    assert "Timings by scenario" in summary
    assert "reld | native | reld | na" in summary


def test_readme_block_is_generated_from_the_target_manifest(tmp_path):
    readme = tmp_path / "README.md"
    readme.write_text(
        "before\n<!-- BENCHMARK:BEGIN -->\n" + render_readme_block() + "<!-- BENCHMARK:END -->\nafter\n",
        encoding="utf-8",
    )
    assert [target for target, _ in BENCHMARK_TARGETS] == [
        "x86_64-linux",
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
    ]
    assert check_readme_block(readme)
    readme.write_text(readme.read_text(encoding="utf-8").replace("macOS", "mac os"), encoding="utf-8")
    assert not check_readme_block(readme)
    write_readme_block(readme)
    assert check_readme_block(readme)
    # Prose is generated too: an extra sentence is just as much drift as a missing panel.
    readme.write_text(readme.read_text(encoding="utf-8").replace("current generation time", "old generation time"), encoding="utf-8")
    assert not check_readme_block(readme)


def test_freshness_guard_rejects_wrong_source_sha_and_accepts_current_outputs(tmp_path, monkeypatch):
    monkeypatch.setenv("GITHUB_SHA", "current")
    for target, _ in BENCHMARK_TARGETS:
        target_report = parse_benchmark_log(SAMPLE_LOG.replace("x86_64-linux", target))
        write_outputs(target_report, collect_metadata(target_report), tmp_path / target)
    assert verify_current_outputs(tmp_path, "current", 60) == []
    errors = verify_current_outputs(tmp_path, "different", 60)
    assert len(errors) == len(BENCHMARK_TARGETS)
    assert all("source SHA" in error for error in errors)


def test_remote_freshness_accepts_injectable_current_results_and_rejects_stale_or_pending(tmp_path, monkeypatch):
    monkeypatch.setenv("GITHUB_SHA", "current")
    for target, _ in BENCHMARK_TARGETS:
        series = EXPECTED_SERIES[target]
        lines = [f"## Link Benchmark: {target}", "", "| Scenario | " + " | ".join(series) + " |"]
        lines.append("|:---------|" + "|".join("---:" for _ in series) + "|")
        lines.extend(f"| {scenario} | " + " | ".join("0.0200" for _ in series) + " |" for scenario in CANONICAL_SCENARIOS)
        report = parse_benchmark_log("\n".join(lines))
        write_outputs(report, collect_metadata(report), tmp_path / target)
    now = __import__("time").time()

    def fetch(url):
        target = url.split("/")[-2]
        return json.loads((tmp_path / target / "latest.json").read_text(encoding="utf-8"))

    assert (
        verify_remote_freshness(
            "https://example.invalid/stats",
            60,
            expected_sha="current",
            fetch_json=fetch,
            now=now,
        )
        == []
    )
    stale = fetch("https://example.invalid/stats/x86_64-linux/latest.json")
    stale["metadata"]["generated_at"] = "2000-01-01T00:00:00Z"

    def stale_fetch(url):
        return stale if "x86_64-linux" in url else fetch(url)

    assert any("old" in error for error in verify_remote_freshness("local", 60, fetch_json=stale_fetch, now=now))
    pending = fetch("https://example.invalid/stats/x86_64-linux/latest.json")
    pending["results"][0]["status"] = "pending"
    assert any("not measured" in error for error in verify_remote_freshness("local", 60, fetch_json=lambda url: pending if "x86_64-linux" in url else fetch(url), now=now))
    unexpected = fetch("https://example.invalid/stats/x86_64-linux/latest.json")
    unexpected["results"][0]["scenario"] = "tiny (1 unit)"
    assert any("unexpected synthetic scenario" in error for error in verify_remote_freshness("local", 60, fetch_json=lambda url: unexpected if "x86_64-linux" in url else fetch(url), now=now))
    wrong_sha = fetch("https://example.invalid/stats/x86_64-linux/latest.json")
    wrong_sha["metadata"]["git_sha"] = "old"
    assert any(
        "source SHA" in error
        for error in verify_remote_freshness(
            "local",
            60,
            expected_sha="current",
            fetch_json=lambda url: wrong_sha if "x86_64-linux" in url else fetch(url),
            now=now,
        )
    )
    wrong_engine = fetch("https://example.invalid/stats/x86_64-pc-windows-msvc/latest.json")
    reld = next(result for result in wrong_engine["results"] if result["series"] == "reld")
    reld["mode"] = "native"
    reld["engine"] = "reld"
    errors = verify_remote_freshness(
        "local",
        60,
        fetch_json=lambda url: wrong_engine if "windows-msvc" in url else fetch(url),
        now=now,
    )
    assert any("mode" in error for error in errors)
    assert any("engine" in error for error in errors)


def test_history_appends_and_caps(tmp_path):
    r = parse_benchmark_log(SAMPLE_LOG)
    meta = collect_metadata(r)
    write_outputs(r, meta, tmp_path)
    write_outputs(r, meta, tmp_path)
    lines = (tmp_path / "history.jsonl").read_text().strip().splitlines()
    assert len(lines) == 2
    for line in lines:
        entry = json.loads(line)
        assert "target" in entry


def test_render_refuses_empty(tmp_path):
    r = parse_benchmark_log("nothing")
    with pytest.raises(SystemExit):
        write_outputs(r, collect_metadata(r), tmp_path)


PENDING_LOG = """
## Link Benchmark: aarch64-apple-darwin

| Scenario | ld | ld64.lld | reld |
|:---------|---:|---------:|----:|
| small (16 units) | 0.0300 | 0.0120 | pending |
| medium (128 units) | 0.1500 | 0.0400 | pending |
| large (512 units) | 0.6000 | 0.1600 | pending |

<!-- linker reld pending: reld bridge measurement pending (rustc-based); see #17 -->
"""


def test_pending_cells_parse_as_pending_not_na():
    r = parse_benchmark_log(PENDING_LOG)
    # A pending cell has no timing (like n/a) but is flagged distinctly.
    assert r.value("small (16 units)", "reld") is None
    assert r.is_pending("small (16 units)", "reld") is True
    # A real n/a would not be flagged pending.
    assert r.is_pending("small (16 units)", "ld") is False


def test_series_status_distinguishes_pending_measured_and_na():
    r = parse_benchmark_log(PENDING_LOG)
    assert r.series_status("ld") == "measured"
    assert r.series_status("ld64.lld") == "measured"
    assert r.series_status("reld") == "pending"
    # A series absent from the table is treated as na, never pending.
    assert parse_benchmark_log(SAMPLE_LOG).series_status("reld") == "na"


def test_latest_json_carries_pending_status(tmp_path):
    r = parse_benchmark_log(PENDING_LOG)
    write_outputs(r, collect_metadata(r), tmp_path)
    payload = json.loads((tmp_path / "latest.json").read_text())
    reld = [e for e in payload["results"] if e["series"] == "reld"]
    assert reld and all(e["status"] == "pending" for e in reld)
    assert all(e["seconds"] is None for e in reld)


def test_html_marks_pending_distinctly():
    from ci.benchmark_stats import render_html

    report = parse_benchmark_log(PENDING_LOG)
    html = render_html(report, collect_metadata(report))
    assert 'class="pending">pending<' in html
    assert "n/a" not in html  # every reld cell is pending here, no failed cells
