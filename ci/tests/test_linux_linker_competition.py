"""Focused contract tests for the Linux competitive linker replay."""

from __future__ import annotations

import hashlib
import io
import json
import tarfile
from pathlib import Path

import pytest

from ci import linux_linker_competition as competition


def _sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _lock(entries: dict[str, dict[str, object]]) -> dict[str, object]:
    return {"schema_version": 1, "comparators": entries}


def _entry(*, archive: bytes = b"archive", binary: bytes = b"binary") -> dict[str, object]:
    return {
        "url": "https://example.invalid/releases/download/v1/tool.tar.gz",
        "archive_sha256": _sha(archive),
        "binary_path": "bin/tool",
        "binary_sha256": _sha(binary),
        "version_argv": ["--version"],
        "version_stdout": "tool 1.0\n",
        "version_stderr": "",
        "recipe": {"remove_arguments": ["--no-fork"], "extra_arguments": []},
    }


def _archive(path: Path, member: str, payload: bytes) -> bytes:
    with tarfile.open(path, "w:gz") as archive:
        info = tarfile.TarInfo(member)
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    return path.read_bytes()


def _raw_sample(label: str, *, round_id: int, position: int) -> dict[str, object]:
    return {
        "contender": label,
        "round": round_id,
        "position": position,
        "order": list(competition.CONTENDER_ORDER),
        "wall_seconds": 1.0 + position / 10 + round_id / 100,
        "peak_rss_kib": 100.0 + position + round_id,
        "cgroup_memory_peak_bytes": 200 + position + round_id,
        "cgroup_cpu_usec": 300 + position + round_id,
        "metric_backend": {
            "wall_seconds": competition.WALL_CLOCK_BACKEND,
            "peak_rss_kib": competition.RSS_BACKEND,
        },
        "output_sha256": "a" * 64,
    }


def _renderer_report(tmp_path: Path) -> dict[str, object]:
    tmp_path.mkdir(parents=True, exist_ok=True)
    contenders: dict[str, Path] = {}
    for label in competition.CONTENDER_ORDER:
        path = tmp_path / label
        path.write_bytes(label.encode())
        contenders[label] = path
    raw_samples = [
        _raw_sample(label, round_id=round_id, position=position)
        for round_id in range(10)
        for position, label in enumerate(competition.CONTENDER_ORDER)
    ]
    return competition.build_report(
        contenders=contenders,
        raw_samples=raw_samples,
        identity={"reld_identity": {"sha256": "a" * 64}, "comparators": {}},
        plan=competition.round_plan(samples=10, warmups=2, seed=103),
        provenance={"corpus_lock": {"sha256": "b" * 64}},
    )


def test_comparator_lock_requires_all_fixed_contenders_and_immutable_fields() -> None:
    entries = {name: _entry() for name in competition.EXTERNAL_CONTENDERS}
    competition.validate_comparator_lock(_lock(entries))

    floating = _lock(entries)
    floating["comparators"] = dict(entries)
    floating["comparators"]["bfd"] = {**_entry(), "url": "https://example.invalid/latest/tool.tar.gz"}
    with pytest.raises(competition.CompetitionError, match="immutable release URL"):
        competition.validate_comparator_lock(floating)

    missing = _lock({name: _entry() for name in competition.EXTERNAL_CONTENDERS[:-1]})
    with pytest.raises(competition.CompetitionError, match="must contain exactly"):
        competition.validate_comparator_lock(missing)

    placeholders = _lock({name: {**_entry(), "archive_sha256": "0" * 64} for name in competition.EXTERNAL_CONTENDERS})
    with pytest.raises(competition.CompetitionError, match="unpublished placeholder"):
        competition.validate_comparator_lock(placeholders)

    bad_recipe = _lock({name: {**_entry(), "recipe": {"remove_arguments": [], "extra_arguments": ["--threads=99"]}} for name in competition.EXTERNAL_CONTENDERS})
    with pytest.raises(competition.CompetitionError, match="exact default-fork recipe"):
        competition.validate_comparator_lock(bad_recipe)


def test_provision_verifies_archive_binary_and_exact_version(tmp_path: Path) -> None:
    staged = tmp_path / "tool.tar.gz"
    payload = b"#!/bin/sh\nprintf 'tool 1.0\\n'\n"
    archive_bytes = _archive(staged, "bin/tool", payload)
    entry = _entry(archive=archive_bytes, binary=payload)
    lock = _lock({name: entry for name in competition.EXTERNAL_CONTENDERS})

    def fetch(url: str) -> bytes:
        assert url == entry["url"]
        return archive_bytes

    paths = competition.provision_comparators(lock, tmp_path / "out", fetch=fetch)
    assert tuple(paths) == competition.EXTERNAL_CONTENDERS
    assert paths["mold"].read_bytes() == payload

    bad = _lock({name: {**entry, "binary_sha256": "1" * 64} for name in competition.EXTERNAL_CONTENDERS})
    with pytest.raises(competition.CompetitionError, match="binary SHA-256 mismatch"):
        competition.provision_comparators(bad, tmp_path / "bad", fetch=fetch)


def test_identity_gate_requires_four_reld_bytes_and_two_per_external(tmp_path: Path) -> None:
    calls: list[str] = []

    def link(label: str, output: Path) -> None:
        calls.append(label)
        output.write_bytes(b"reld" if label in {"baseline", "candidate"} else label.encode())

    result = competition.identity_gate(
        {name: Path(name) for name in competition.CONTENDER_ORDER},
        link=link,
        native_oracle=lambda output: output.is_file(),
        artifact_dir=tmp_path,
    )
    assert calls == ["baseline", "baseline", "candidate", "candidate", "bfd", "bfd", "lld", "lld", "mold", "mold", "wild", "wild"]
    assert result["reld_identity"]["sha256"] == _sha(b"reld")
    assert result["comparators"]["wild"]["sha256"] == _sha(b"wild")


def test_identity_gate_retains_first_offset_on_candidate_delta(tmp_path: Path) -> None:
    def link(label: str, output: Path) -> None:
        output.write_bytes(b"abc" if label != "candidate" else b"axc")

    with pytest.raises(competition.CompetitionError, match="first differing offset 1"):
        competition.identity_gate(
            {name: Path(name) for name in competition.CONTENDER_ORDER},
            link=link,
            native_oracle=lambda output: True,
            artifact_dir=tmp_path,
        )
    assert {path.name for path in tmp_path.iterdir()} >= {"baseline-1", "baseline-2", "candidate-1", "candidate-2", "identity-failure.json"}
    assert json.loads((tmp_path / "identity-failure.json").read_text())["first_differing_offset"] == 1


def test_seeded_round_plan_requires_two_warmups_ten_rounds_and_balances_positions() -> None:
    with pytest.raises(competition.CompetitionError, match="at least two warmups"):
        competition.round_plan(samples=10, warmups=1, seed=103)
    with pytest.raises(competition.CompetitionError, match="at least ten"):
        competition.round_plan(samples=9, warmups=2, seed=103)
    plan = competition.round_plan(samples=12, warmups=2, seed=103)
    assert len(plan["warmups"]) == 2
    assert len(plan["rounds"]) == 12
    for contender in competition.CONTENDER_ORDER:
        positions = [row["order"].index(contender) for row in plan["rounds"]]
        assert max(positions.count(position) for position in set(positions)) - min(positions.count(position) for position in set(positions)) <= 1


def test_paired_bootstrap_is_deterministic_and_never_pools_mismatched_rounds() -> None:
    baseline = [{"round": n, "wall_seconds": 10.0, "peak_rss_kib": 100.0} for n in range(10)]
    candidate = [{"round": n, "wall_seconds": 8.0, "peak_rss_kib": 90.0} for n in range(10)]
    first = competition.paired_comparison(baseline, candidate, seed=103)
    second = competition.paired_comparison(baseline, candidate, seed=103)
    assert first == second
    assert first["wall_seconds"]["improvement_fraction"] == pytest.approx(0.2)
    with pytest.raises(competition.CompetitionError, match="same round ids"):
        competition.paired_comparison(baseline, candidate[:-1], seed=103)


def test_sample_validation_requires_whole_tree_metrics_and_identity_hash() -> None:
    sample = {
        "contender": "candidate",
        "round": 0,
        "position": 0,
        "order": list(competition.CONTENDER_ORDER),
        "wall_seconds": 0.1,
        "peak_rss_kib": 1.0,
        "cgroup_memory_peak_bytes": 2,
        "cgroup_cpu_usec": 3,
        "metric_backend": {
            "wall_seconds": competition.WALL_CLOCK_BACKEND,
            "peak_rss_kib": competition.RSS_BACKEND,
        },
        "output_sha256": "a" * 64,
    }
    competition.validate_sample(sample)
    sample["metric_backend"] = {"wall_seconds": competition.WALL_CLOCK_BACKEND, "peak_rss_kib": "gnu-time-parent"}
    with pytest.raises(competition.CompetitionError, match="whole-tree"):
        competition.validate_sample(sample)


def test_report_uses_the_renderer_canonical_contender_summary_and_comparison_schema(tmp_path: Path) -> None:
    report = _renderer_report(tmp_path)

    assert report["contender_order"] == list(competition.CONTENDER_ORDER)
    assert list(report["contenders"]) == list(competition.CONTENDER_ORDER)
    assert set(report) >= {"contender_order", "contenders", "comparisons", "raw_samples", "identity", "provenance", "metric_scope"}
    for label, contender in report["contenders"].items():
        assert contender["label"] == label
        assert set(contender) == {"label", "path", "sha256", "summaries"}
        for metric in ("wall_seconds", "peak_rss_kib"):
            summary = contender["summaries"][metric]
            assert set(summary) == {"median", "median_absolute_deviation", "min", "max", "bootstrap_95_ci"}
            lower, upper = summary["bootstrap_95_ci"]
            assert lower <= summary["median"] <= upper
            assert summary["min"] <= lower <= upper <= summary["max"]
    assert [comparison["reference"] for comparison in report["comparisons"]] == [*competition.EXTERNAL_CONTENDERS, "baseline"]
    for comparison in report["comparisons"]:
        assert set(comparison) == {"reference", "candidate", "metrics"}
        assert comparison["candidate"] == "candidate"
        assert set(comparison["metrics"]) == {"wall_seconds", "peak_rss_kib"}
    sample = report["raw_samples"][0]
    assert set(sample) >= {"round", "position", "contender", "wall_seconds", "peak_rss_kib", "output_sha256", "metric_backend", "order", "cgroup_memory_peak_bytes", "cgroup_cpu_usec"}


def test_write_evidence_creates_atomic_renderer_sidecars_with_jsonl_parity(tmp_path: Path) -> None:
    report = _renderer_report(tmp_path / "binaries")
    report_path = tmp_path / "evidence" / "report.json"
    competition.write_evidence(report, report_path)

    assert json.loads(report_path.read_text()) == report
    raw_samples = [json.loads(line) for line in (report_path.parent / "raw-samples.jsonl").read_text().splitlines()]
    assert raw_samples == report["raw_samples"]
    assert json.loads((report_path.parent / "provenance.json").read_text()) == report["provenance"]
    assert not list(report_path.parent.glob(".*.tmp"))
