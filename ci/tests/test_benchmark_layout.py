"""Benchmark charts stay vertically stacked at full width (reld#54).

The README block and the combined page are both generated, and the README check compares the file
against the same generator that wrote it — so a change back to a column of thumbnails would pass
that check with both sides agreeing. These tests assert the property itself: one chart per row,
full width, one per OS, in the manifest's order, and no multi-column layout holding them.
"""

from __future__ import annotations

import re
from pathlib import Path

from ci.benchmark_stats import BENCHMARK_TARGETS
from ci.benchmark_stats import render_combined_index
from ci.benchmark_stats import render_readme_block
from ci.benchmark_stats import write_combined_index

REPO_ROOT = Path(__file__).resolve().parents[2]


def test_readme_block_is_one_full_width_chart_per_os() -> None:
    block = render_readme_block()
    panels = re.findall(r'<p align="center">.*?</p>', block, re.S)
    assert len(panels) == len(BENCHMARK_TARGETS)
    for panel, (target, _title) in zip(panels, BENCHMARK_TARGETS, strict=True):
        assert target in panel, panel
        assert 'width="100%"' in panel, panel
    # A table is how the thumbnails came back last time.
    assert "<table" not in block


def test_readme_file_itself_carries_the_stacked_block() -> None:
    readme = (REPO_ROOT / "README.md").read_text(encoding="utf-8")
    block = readme.split("<!-- BENCHMARK:BEGIN -->")[1].split("<!-- BENCHMARK:END -->")[0]
    assert "<table" not in block
    assert block.count('width="100%"') == len(BENCHMARK_TARGETS)


def test_combined_index_stacks_every_target_full_width() -> None:
    page = render_combined_index("2026-01-01 00:00:00 UTC")
    sections = re.findall(r'<section class="chart">.*?</section>', page, re.S)
    assert len(sections) == len(BENCHMARK_TARGETS)
    for section, (target, title) in zip(sections, BENCHMARK_TARGETS, strict=True):
        assert f"<h2>{target}</h2>" in section
        assert f'href="./{target}/"' in section, section
        assert title in section
    # Full width, one per row: a grid or a float would defeat the point.
    assert "section.chart img{display:block;width:100%" in page
    assert "<table" not in page
    assert "grid-template-columns" not in page
    assert "float:" not in page


def test_combined_index_is_written_above_the_target_directories(tmp_path: Path) -> None:
    path = write_combined_index(tmp_path, "2026-01-01 00:00:00 UTC")
    assert path == tmp_path / "index.html"
    written = path.read_text(encoding="utf-8")
    for target, _title in BENCHMARK_TARGETS:
        # Relative, so the page works on the published branch and in a downloaded artifact.
        assert f'src="./{target}/' in written
