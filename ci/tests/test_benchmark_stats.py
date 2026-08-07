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
    assert payload["schema_version"] == 2
    assert len(payload["results"]) == 15
    for entry in payload["results"]:
        assert "mode" in entry
        if entry["series"] in ("bfd", "lld"):
            assert entry["mode"] == "reference"


def test_series_mode():
    assert series_mode("reld", "Linux") == "native"
    assert series_mode("reld", "Windows") == "bridge"
    assert series_mode("reld", "Darwin") == "bridge"
    assert series_mode("bfd", "Linux") == "reference"
    assert series_mode("lld", "Windows") == "reference"


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
