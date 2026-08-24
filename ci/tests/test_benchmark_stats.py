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
    BENCHMARK_ID,
    BENCHMARK_TARGETS,
    CANONICAL_SCENARIOS,
    EXPECTED_SERIES,
    check_readme_block,
    collect_metadata,
    parse_benchmark_log,
    read_metadata,
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
| no-LTO | 1.2100 | 0.8100 | 0.7200 | 1.1000 | 1.3000 |
| ThinLTO | 1.1400 | 0.7900 | 0.7100 | 1.0500 | 1.2500 |
| full-LTO | 1.1200 | 0.7600 | 0.6900 | 1.0000 | 1.2000 |

## Linker Startup: x86_64-linux

| Linker | Seconds |
|:-------|--------:|
| bfd | 0.0100 |
| lld | 0.0080 |
| mold | 0.0060 |
| wild | 0.0050 |
| reld | 0.0950 |
"""


def sample_log_for_target(target: str) -> str:
    series = EXPECTED_SERIES[target]
    startup = {name: 0.0500 for name in series}
    reference = {
        "x86_64-linux": "wild",
        "x86_64-pc-windows-msvc": "lld",
        "aarch64-apple-darwin": "ld64.lld",
    }[target]
    startup[reference] = 0.0050
    lines = [
        f"## Link Benchmark: {target}",
        "",
        "| Configuration | " + " | ".join(series) + " |",
        "|:--------------|" + "|".join("----:" for _ in series) + "|",
        *(f"| {scenario} | " + " | ".join("1.0000" for _ in series) + " |" for scenario in CANONICAL_SCENARIOS),
        "",
        f"## Linker Startup: {target}",
        "",
        "| Linker | Seconds |",
        "|:-------|--------:|",
        *(f"| {name} | {startup[name]:.4f} |" for name in series),
    ]
    return "\n".join(lines)


def test_parses_label_and_series():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.label == "x86_64-linux"
    assert r.series == ["bfd", "lld", "mold", "wild", "reld"]


def test_parses_scenarios_in_order():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.scenarios() == [
        "no-LTO",
        "ThinLTO",
        "full-LTO",
    ]


def test_parses_values_and_na():
    r = parse_benchmark_log(SAMPLE_LOG)
    assert r.value("no-LTO", "bfd") == pytest.approx(1.2100)
    assert r.value("full-LTO", "wild") == pytest.approx(1.0000)
    # The unimplemented column must survive as None, not as 0.0 — a zero would render as
    # an infinitely fast linker, which is exactly the kind of accidental lie this guards.
    assert r.value("no-LTO", "reld") == pytest.approx(1.3000)


def test_parses_linker_startup_separately_from_final_links():
    report = parse_benchmark_log(SAMPLE_LOG)

    assert report.startup_seconds == {
        "bfd": 0.0100,
        "lld": 0.0080,
        "mold": 0.0060,
        "wild": 0.0050,
        "reld": 0.0950,
    }


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
    assert payload["schema_version"] == 6
    assert payload["benchmark_id"] == BENCHMARK_ID
    assert payload["metadata"]["benchmark"]["workload"] == "ci/e2e/link-workload"
    assert payload["startup"] == [
        {"series": "bfd", "seconds": 0.01, "status": "measured", "mode": "reference", "engine": "bfd"},
        {"series": "lld", "seconds": 0.008, "status": "measured", "mode": "reference", "engine": "lld"},
        {"series": "mold", "seconds": 0.006, "status": "measured", "mode": "reference", "engine": "mold"},
        {"series": "wild", "seconds": 0.005, "status": "measured", "mode": "reference", "engine": "wild"},
        {"series": "reld", "seconds": 0.095, "status": "measured", "mode": "native", "engine": "reld"},
    ]
    assert len(payload["results"]) == 15
    for entry in payload["results"]:
        assert "mode" in entry
        assert "engine" in entry
        assert "status" in entry
        if entry["series"] in ("bfd", "lld"):
            assert entry["mode"] == "reference"
            assert entry["status"] == "measured"
        if entry["series"] == "reld":
            assert entry["status"] == "measured"


def test_native_runner_metadata_round_trips_and_targets_its_image(tmp_path):
    report = parse_benchmark_log(SAMPLE_LOG)
    meta = collect_metadata(report)
    path = tmp_path / "metadata.json"
    path.write_text(json.dumps(meta), encoding="utf-8")

    loaded = read_metadata(path, report)

    assert loaded["runner"] == meta["runner"]
    assert loaded["versions"] == meta["versions"]
    assert loaded["raw_image_url"].endswith("/x86_64-linux/benchmark-link.jpg")


def test_native_runner_metadata_rejects_a_different_target(tmp_path):
    report = parse_benchmark_log(SAMPLE_LOG)
    meta = collect_metadata(report)
    meta["target"] = "aarch64-apple-darwin"
    path = tmp_path / "metadata.json"
    path.write_text(json.dumps(meta), encoding="utf-8")

    with pytest.raises(ValueError, match="does not match"):
        read_metadata(path, report)


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
    assert "Timings by configuration" in summary
    assert "reld | native | reld | measured | no-LTO: 1.3000" in summary
    assert "Fixed linker startup (reported raw; never subtracted)" in summary


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
        target_report = parse_benchmark_log(sample_log_for_target(target))
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
        lines.extend(f"| {scenario} | " + " | ".join("2.0000" for _ in series) + " |" for scenario in CANONICAL_SCENARIOS)
        lines.extend(
            [
                "",
                f"## Linker Startup: {target}",
                "",
                "| Linker | Seconds |",
                "|:-------|--------:|",
                *(f"| {name} | 0.0500 |" for name in series),
            ]
        )
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
    unexpected["results"][0]["configuration"] = "tiny (1 unit)"
    assert any("unexpected configuration" in error for error in verify_remote_freshness("local", 60, fetch_json=lambda url: unexpected if "x86_64-linux" in url else fetch(url), now=now))
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
        assert entry["schema_version"] == 6
        assert entry["benchmark_id"] == BENCHMARK_ID


def test_history_drops_incompatible_size_based_generation(tmp_path):
    history = tmp_path / "history.jsonl"
    history.write_text('{"target":"x86_64-linux","results":[{"scenario":"large (512 units)"}]}\n')
    report = parse_benchmark_log(SAMPLE_LOG)

    write_outputs(report, collect_metadata(report), tmp_path)

    lines = history.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 1
    entry = json.loads(lines[0])
    assert entry["benchmark_id"] == BENCHMARK_ID
    assert {result["configuration"] for result in entry["results"]} == set(CANONICAL_SCENARIOS)


def test_render_refuses_empty(tmp_path):
    r = parse_benchmark_log("nothing")
    with pytest.raises(SystemExit):
        write_outputs(r, collect_metadata(r), tmp_path)


PENDING_LOG = """
## Link Benchmark: aarch64-apple-darwin

| Scenario | ld | ld64.lld | reld |
|:---------|---:|---------:|----:|
| no-LTO | 0.0300 | 0.0120 | pending |
| ThinLTO | 0.1500 | 0.0400 | pending |
| full-LTO | 0.6000 | 0.1600 | pending |

<!-- linker reld pending: reld bridge measurement pending (rustc-based); see #17 -->
"""


def test_pending_cells_parse_as_pending_not_na():
    r = parse_benchmark_log(PENDING_LOG)
    # A pending cell has no timing (like n/a) but is flagged distinctly.
    assert r.value("no-LTO", "reld") is None
    assert r.is_pending("no-LTO", "reld") is True
    # A real n/a would not be flagged pending.
    assert r.is_pending("no-LTO", "ld") is False


def test_series_status_distinguishes_pending_measured_and_na():
    r = parse_benchmark_log(PENDING_LOG)
    assert r.series_status("ld") == "measured"
    assert r.series_status("ld64.lld") == "measured"
    assert r.series_status("reld") == "pending"
    # A series absent from the table is treated as na, never pending.
    assert parse_benchmark_log(SAMPLE_LOG).series_status("gold") == "na"


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
