"""Benchmark aggregation is invokable and testable outside Actions (reld#48).

The aggregate job used to be a shell loop that knew the target list, the artifact layout, and the
history carry-forward. These tests run that orchestration directly: three targets rendered into
their own directories from downloaded logs, no network, no credentials, and a clear failure when
an input is missing or unparseable.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ci.benchmark_pipeline import PipelineError
from ci.benchmark_pipeline import REQUIRED_ARTIFACTS
from ci.benchmark_pipeline import carry_forward_history
from ci.benchmark_pipeline import main
from ci.benchmark_pipeline import run
from ci.benchmark_pipeline import target_inputs
from ci.benchmark_stats import BENCHMARK_TARGETS
from ci.tests.test_benchmark_stats import sample_log_for_target


def write_inputs(input_root: Path, *, targets: tuple[str, ...] | None = None) -> Path:
    """Lay out `benchmark-log-<target>/` exactly as download-artifact does."""
    names = targets if targets is not None else tuple(target for target, _ in BENCHMARK_TARGETS)
    for target in names:
        directory = input_root / f"benchmark-log-{target}"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "benchmark.log").write_text(sample_log_for_target(target), encoding="utf-8")
        (directory / "metadata.json").write_text(
            json.dumps(
                {
                    "generated_at": "2026-01-01T00:00:00Z",
                    "repository": "zackees/reld",
                    "git_sha": "0" * 40,
                    "target": target,
                    "runner": {"os": "Linux", "platform": "test"},
                }
            ),
            encoding="utf-8",
        )
    return input_root


def test_every_target_renders_into_its_own_directory(tmp_path: Path) -> None:
    input_root = write_inputs(tmp_path / "in")
    output_root = tmp_path / "out"

    run(input_root, output_root, base_url=None)

    for target, _title in BENCHMARK_TARGETS:
        for name in REQUIRED_ARTIFACTS:
            assert (output_root / target / name).is_file(), f"{target}/{name}"
        # Target isolation: each latest.json names only its own target.
        payload = json.loads((output_root / target / "latest.json").read_text(encoding="utf-8"))
        assert payload["metadata"]["target"] == target
    # The combined page sits above the per-target directories.
    assert (output_root / "index.html").is_file()


def test_a_missing_raw_log_fails_with_the_target_named(tmp_path: Path) -> None:
    input_root = write_inputs(tmp_path / "in", targets=(BENCHMARK_TARGETS[0][0],))
    missing = BENCHMARK_TARGETS[1][0]

    with pytest.raises(PipelineError) as error:
        run(input_root, tmp_path / "out", base_url=None)

    assert missing in str(error.value)
    assert "no raw benchmark log" in str(error.value)


def test_a_malformed_raw_log_fails_instead_of_publishing_an_empty_chart(tmp_path: Path) -> None:
    input_root = write_inputs(tmp_path / "in")
    target = BENCHMARK_TARGETS[0][0]
    (input_root / f"benchmark-log-{target}" / "benchmark.log").write_text("not a benchmark", encoding="utf-8")

    with pytest.raises(PipelineError, match="parsed no benchmark rows"):
        run(input_root, tmp_path / "out", base_url=None)


def test_metadata_for_the_wrong_target_is_rejected(tmp_path: Path) -> None:
    input_root = write_inputs(tmp_path / "in")
    first, second = BENCHMARK_TARGETS[0][0], BENCHMARK_TARGETS[1][0]
    path = input_root / f"benchmark-log-{first}" / "metadata.json"
    payload = json.loads(path.read_text(encoding="utf-8"))
    payload["target"] = second
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(PipelineError, match="invalid metadata"):
        run(input_root, tmp_path / "out", base_url=None)


def test_history_is_carried_forward_from_a_local_root(tmp_path: Path) -> None:
    target = BENCHMARK_TARGETS[0][0]
    history_root = tmp_path / "published"
    (history_root / target).mkdir(parents=True)
    (history_root / target / "history.jsonl").write_text('{"run": 1}\n', encoding="utf-8")
    destination = tmp_path / "out" / target / "history.jsonl"

    note = carry_forward_history(target, destination, history_root=history_root, base_url=None)

    assert '{"run": 1}' in destination.read_text(encoding="utf-8")
    assert "carried forward" in note


def test_a_target_with_no_published_history_starts_empty_rather_than_failing(tmp_path: Path) -> None:
    target = BENCHMARK_TARGETS[0][0]
    destination = tmp_path / "out" / target / "history.jsonl"

    note = carry_forward_history(target, destination, history_root=tmp_path / "absent", base_url=None)

    assert destination.read_text(encoding="utf-8") == ""
    assert "starting empty" in note


def test_local_invocation_needs_no_network_and_publishes_nothing(tmp_path: Path) -> None:
    input_root = write_inputs(tmp_path / "in")
    output_root = tmp_path / "out"

    exit_code = main(
        [
            "--input-root",
            str(input_root),
            "--output-root",
            str(output_root),
            "--no-history-fetch",
        ]
    )

    assert exit_code == 0
    assert (output_root / BENCHMARK_TARGETS[0][0] / "latest.json").is_file()
    # Nothing that could publish: no git directory, no remote state, only rendered artifacts.
    assert not (output_root / ".git").exists()


def test_missing_input_reports_failure_through_the_cli(tmp_path: Path) -> None:
    exit_code = main(
        [
            "--input-root",
            str(tmp_path / "empty"),
            "--output-root",
            str(tmp_path / "out"),
            "--no-history-fetch",
        ]
    )
    assert exit_code == 1


def test_artifact_layout_matches_what_download_artifact_produces() -> None:
    inputs = target_inputs(Path("benchmark-input"), "x86_64-linux")
    assert inputs.log == Path("benchmark-input/benchmark-log-x86_64-linux/benchmark.log")
    assert inputs.metadata == Path("benchmark-input/benchmark-log-x86_64-linux/metadata.json")
