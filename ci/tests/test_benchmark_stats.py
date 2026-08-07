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
    collect_metadata,
    parse_benchmark_log,
    series_mode,
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
    assert payload["schema_version"] == 3
    assert len(payload["results"]) == 15
    for entry in payload["results"]:
        assert "mode" in entry
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
