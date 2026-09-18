from __future__ import annotations

import re
from pathlib import Path

import pytest

from ci.cross_ship_metrics import CacheMetrics
from ci.cross_ship_metrics import format_table
from ci.cross_ship_metrics import format_wall_time
from ci.cross_ship_metrics import main
from ci.cross_ship_metrics import normalize


def test_normalize_missing_or_blank_is_unknown() -> None:
    assert normalize(None) == "unknown"
    assert normalize("") == "unknown"
    assert normalize("  ") == "unknown"


def test_normalize_preserves_present_value() -> None:
    assert normalize("true") == "true"


def test_format_wall_time_renders_minutes_and_seconds() -> None:
    assert format_wall_time("125") == "2m05s"
    assert format_wall_time("45") == "0m45s"


def test_format_wall_time_unknown_for_missing_or_invalid() -> None:
    assert format_wall_time(None) == "unknown"
    assert format_wall_time("") == "unknown"
    assert format_wall_time("n/a") == "unknown"
    assert format_wall_time("-1") == "unknown"


def test_format_table_with_every_field_unknown_does_not_fabricate() -> None:
    metrics = CacheMetrics(
        target="windows-msvc",
        triple="x86_64-pc-windows-msvc",
        cache_hit=normalize(None),
        build_cache_hit=normalize(None),
        target_cache_hit=normalize(None),
        compile_cache_hits=normalize(None),
        compile_cache_misses=normalize(None),
        compile_cache_hit_rate=normalize(None),
        wall_time=format_wall_time(None),
    )

    table = format_table(metrics)

    assert "unknown" in table
    assert not re.search(r"\d+%", table)


def test_main_prints_target_and_supplied_values(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)

    assert (
        main(
            [
                "--target",
                "linux-gnu",
                "--triple",
                "x86_64-unknown-linux-gnu",
                "--cache-hit",
                "true",
                "--build-cache-hit",
                "false",
                "--target-cache-hit",
                "true",
                "--compile-cache-hits",
                "42",
                "--compile-cache-misses",
                "3",
                "--compile-cache-hit-rate",
                "93%",
                "--wall-seconds",
                "125",
            ]
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "linux-gnu" in output
    assert "x86_64-unknown-linux-gnu" in output
    assert "true" in output
    assert "false" in output
    assert "42" in output
    assert "3" in output
    assert "93%" in output
    assert "2m05s" in output


def test_main_appends_table_to_github_step_summary(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    summary = tmp_path / "summary.md"
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(summary))

    assert (
        main(
            [
                "--target",
                "macos-aarch64",
                "--triple",
                "aarch64-apple-darwin",
                "--cache-hit",
                "true",
            ]
        )
        == 0
    )
    contents = summary.read_text(encoding="utf-8")
    assert "### soldr cache" in contents
    assert "macos-aarch64" in contents


def test_main_degrades_cleanly_with_no_optional_flags(capsys: pytest.CaptureFixture[str]) -> None:
    assert (
        main(
            [
                "--target",
                "windows-msvc",
                "--triple",
                "x86_64-pc-windows-msvc",
            ]
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "windows-msvc" in output
    assert output.count("unknown") >= 7
