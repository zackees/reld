import hashlib
import json
import subprocess
import tarfile
from pathlib import Path

import pytest

import ci.clang_link_replay as replay_module
from ci.clang_link_replay import (
    ReplayError,
    _identity_gate,
    _link_and_validate,
    acquire_archive,
    extract_and_verify,
    first_differing_offset,
    link_recipe,
    native_oracle,
    run_replay,
    validate_lock,
    verify_input_closure,
)


def _digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _lock(
    files: dict[str, bytes],
    *,
    archive_sha256: str | None = None,
    archive_bytes: int = 1,
    archive_url: str | None = None,
) -> dict[str, object]:
    return {
        "schema_version": 1,
        "platform": "x86_64-unknown-linux-gnu",
        "source": {
            "repository": "https://github.com/llvm/llvm-project.git",
            "tag": "llvmorg-22.1.8",
            "tag_object": "a" * 40,
            "peeled_commit": "b" * 40,
        },
        "builder": {
            "image": f"example.invalid/builder@sha256:{'c' * 64}",
            "packages": ["clang=22.1.8-1", "cmake=4.0.0-1"],
            "configure_argv": ["cmake", "-G", "Ninja"],
            "build_argv": ["ninja", "clang"],
            "toolchain": {"cc": "clang version 22.1.8", "ld": "GNU ld 2.44"},
        },
        "archive": {
            "url": archive_url,
            "sha256": archive_sha256 or "d" * 64,
            "bytes": archive_bytes,
        },
        "link": {
            "arguments": [f"@{{CORPUS}}/link.rsp", "-o", "{OUTPUT}"],
            "cwd": ".",
            "environment": {"LC_ALL": "C", "PATH": "/usr/bin"},
            "stdout_utf8": "",
            "stderr_utf8": "",
            "response_files": ["link.rsp"],
        },
        "oracle": {
            "arguments": ["{OUTPUT}", "--version"],
            "exit_code": 0,
            "stdout_utf8": "clang version pinned\n",
            "stderr_utf8": "",
        },
        "replay": {
            "target_seconds": 30.0,
            "warmup_runs": 1,
            "identity_runs_per_side": 2,
        },
        "files": [
            {"path": path, "sha256": _digest(data), "bytes": len(data)}
            for path, data in sorted(files.items())
        ],
    }


def _write_corpus(root: Path, files: dict[str, bytes]) -> None:
    for relative, data in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def _archive(tmp_path: Path, files: dict[str, bytes]) -> Path:
    corpus = tmp_path / "source-corpus"
    _write_corpus(corpus, files)
    archive = tmp_path / "clang-link-corpus.tar.gz"
    with tarfile.open(archive, "w:gz") as handle:
        for path in sorted(corpus.rglob("*")):
            if path.is_file():
                handle.add(path, arcname=path.relative_to(corpus))
    return archive


@pytest.fixture
def corpus_files() -> dict[str, bytes]:
    return {"objects/input.o": b"ELF-object", "link.rsp": b"objects/input.o\n"}


def test_lock_requires_pinned_provenance_and_fixed_replay_contract(corpus_files) -> None:
    lock = _lock(corpus_files)
    validate_lock(lock)

    lock["builder"]["image"] = "ubuntu:24.04"
    with pytest.raises(ReplayError, match="pin an image"):
        validate_lock(lock)

    lock = _lock(corpus_files)
    lock["replay"]["target_seconds"] = 29.0
    with pytest.raises(ReplayError, match="exactly 30.0"):
        validate_lock(lock)


def test_lock_requires_exact_output_placeholder_and_complete_response_closure(corpus_files) -> None:
    lock = _lock(corpus_files)
    lock["link"]["arguments"] = ["@{CORPUS}/link.rsp"]
    with pytest.raises(ReplayError, match="exactly once"):
        validate_lock(lock)

    lock = _lock({"objects/input.o": b"ELF-object"})
    with pytest.raises(ReplayError, match="response files are absent"):
        validate_lock(lock)


def test_unpublished_archive_requires_explicit_local_override(tmp_path, corpus_files) -> None:
    lock = _lock(corpus_files)
    validate_lock(lock)
    with pytest.raises(ReplayError, match="pass --archive"):
        acquire_archive(lock, archive_override=None, destination=tmp_path / "download.tar.gz")


def test_local_archive_is_hashed_then_complete_closure_is_verified(tmp_path, corpus_files) -> None:
    archive = _archive(tmp_path, corpus_files)
    lock = _lock(
        corpus_files,
        archive_sha256=replay_module.sha256_file(archive),
        archive_bytes=archive.stat().st_size,
    )
    validate_lock(lock)

    acquired = acquire_archive(
        lock,
        archive_override=archive,
        destination=tmp_path / "unused.tar.gz",
    )
    corpus = tmp_path / "extracted"
    extract_and_verify(lock, acquired, corpus)
    verify_input_closure(lock, corpus)

    (corpus / "unrecorded.a").write_bytes(b"extra")
    with pytest.raises(ReplayError, match=r"extra=\['unrecorded.a'\]"):
        verify_input_closure(lock, corpus)


def test_archive_tampering_fails_before_extraction(tmp_path, corpus_files) -> None:
    archive = _archive(tmp_path, corpus_files)
    lock = _lock(
        corpus_files,
        archive_sha256=replay_module.sha256_file(archive),
        archive_bytes=archive.stat().st_size,
    )
    archive.write_bytes(b"x" * archive.stat().st_size)
    with pytest.raises(ReplayError, match="SHA-256 mismatch"):
        acquire_archive(lock, archive_override=archive, destination=tmp_path / "unused")


@pytest.mark.parametrize(
    ("left", "right", "expected"),
    [(b"same", b"same", None), (b"abc", b"axc", 1), (b"abc", b"abcd", 3)],
)
def test_first_differing_offset_includes_length_mismatch(tmp_path, left, right, expected) -> None:
    left_path = tmp_path / "left"
    right_path = tmp_path / "right"
    left_path.write_bytes(left)
    right_path.write_bytes(right)
    assert first_differing_offset(left_path, right_path) == expected


def test_link_timer_excludes_native_oracle(tmp_path, corpus_files, monkeypatch) -> None:
    corpus = tmp_path / "corpus"
    _write_corpus(corpus, corpus_files)
    lock = _lock(corpus_files)
    recipe = link_recipe(lock)
    oracle = native_oracle(lock)
    output = tmp_path / "output" / "clang"
    output.parent.mkdir()
    calls: list[list[str]] = []

    def fake_run(command, **kwargs):
        calls.append(command)
        assert kwargs["capture_output"] is True
        if len(calls) == 1:
            output.write_bytes(b"ELF-executable")
            return subprocess.CompletedProcess(command, 0, b"", b"")
        return subprocess.CompletedProcess(command, 0, b"clang version pinned\n", b"")

    ticks = iter((10.0, 10.25))
    monkeypatch.setattr(replay_module.subprocess, "run", fake_run)
    elapsed = _link_and_validate(
        tmp_path / "reld",
        output,
        recipe=recipe,
        oracle=oracle,
        corpus_root=corpus,
        clock=lambda: next(ticks),
    )

    assert elapsed == 0.25
    assert calls[0][0] == str(tmp_path / "reld")
    assert calls[1] == [str(output), "--version"]


def test_native_oracle_requires_exact_bytes(tmp_path, corpus_files, monkeypatch) -> None:
    corpus = tmp_path / "corpus"
    _write_corpus(corpus, corpus_files)
    output = tmp_path / "clang"
    output.write_bytes(b"ELF")

    monkeypatch.setattr(
        replay_module.subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 0, b"wrong\n", b""),
    )
    with pytest.raises(ReplayError, match="native oracle mismatch"):
        replay_module._run_native_oracle(
            output,
            oracle=native_oracle(_lock(corpus_files)),
            corpus_root=corpus,
            cwd=corpus,
            environment={},
        )


def test_identity_gate_retains_all_four_outputs_and_first_offset(tmp_path, corpus_files, monkeypatch) -> None:
    corpus = tmp_path / "corpus"
    _write_corpus(corpus, corpus_files)
    output = tmp_path / "output"
    artifacts = tmp_path / "artifacts"
    payloads = iter((b"aa", b"aa", b"ab", b"ab"))

    def fake_link(*args, **kwargs):
        del args
        kwargs.pop("recipe")
        kwargs.pop("oracle")
        kwargs.pop("corpus_root")
        output.write_bytes(next(payloads))
        return 1.0

    monkeypatch.setattr(replay_module, "_link_and_validate", fake_link)
    with pytest.raises(ReplayError, match="offset 1"):
        _identity_gate(
            tmp_path / "baseline",
            tmp_path / "candidate",
            lock=_lock(corpus_files),
            corpus_root=corpus,
            output=output,
            artifact_dir=artifacts,
        )

    assert {path.name for path in artifacts.iterdir()} == {
        "baseline-1",
        "baseline-2",
        "candidate-1",
        "candidate-2",
        "identity-failure.json",
    }
    failure = json.loads((artifacts / "identity-failure.json").read_text())
    assert failure["first_differing_offset"] == 1


def test_replay_uses_one_warmup_to_fix_count_and_times_only_candidate_links(
    tmp_path, corpus_files, monkeypatch
) -> None:
    corpus = tmp_path / "corpus"
    _write_corpus(corpus, corpus_files)
    baseline = tmp_path / "baseline"
    candidate = tmp_path / "candidate"
    baseline.write_bytes(b"baseline-binary")
    candidate.write_bytes(b"candidate-binary")
    report_path = tmp_path / "report.json"
    durations = iter((10.0, 9.0, 10.0, 11.0))
    calls: list[Path] = []

    monkeypatch.setattr(
        replay_module,
        "_identity_gate",
        lambda *args, **kwargs: {"status": "passed", "sha256": {}, "artifacts": []},
    )

    def fake_link(linker, *args, **kwargs):
        del args, kwargs
        calls.append(linker)
        return next(durations)

    monkeypatch.setattr(replay_module, "_link_and_validate", fake_link)
    report = run_replay(
        _lock(corpus_files),
        baseline=baseline,
        candidate=candidate,
        corpus_root=corpus,
        workdir=tmp_path / "work",
        report_path=report_path,
    )

    assert calls == [candidate, candidate, candidate, candidate]
    assert report["calibration_seconds"] == 10.0
    assert report["fixed_replay_count"] == 3
    assert report["sample_seconds"] == [9.0, 10.0, 11.0]
    assert report["timed_total_seconds"] == 30.0
    assert report["timing_scope"] == "final linker subprocess only"
    assert json.loads(report_path.read_text())["native_oracle_after_every_link"] is True
