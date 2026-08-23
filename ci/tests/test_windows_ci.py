import json
import os
from pathlib import Path

import pytest

from ci.windows_ci import (
    WindowsCiError,
    _msvc_linker,
    _msvc_path_env,
    freeze_large_corpus,
    require_generated_table,
    require_measured_row,
)


def _table(row: str) -> str:
    return "\n".join(
        [
            "## Link Benchmark: x86_64-pc-windows-msvc",
            "",
            "| Configuration | link.exe | lld | reld |",
            "|:--------------|---------:|----:|-----:|",
            row,
            "",
        ]
    )


def test_generated_table_requires_three_real_large_timings():
    require_generated_table(_table("| large (512 units) | 1.0000 | 2.0000 | 3.0000 |"))

    with pytest.raises(WindowsCiError, match="must contain real timings"):
        require_generated_table(_table("| large (512 units) | 1.0000 | 2.0000 | n/a |"))


def test_replay_row_accepts_crlf_but_rejects_pending():
    require_measured_row(
        "| large replay (512 units) | 1.0000 | 2.0000 | 3.0000 |\r\n",
        "large replay (512 units)",
    )

    with pytest.raises(WindowsCiError, match="must contain real timings"):
        require_measured_row(
            "| large replay (512 units) | 1.0000 | 2.0000 | pending |\n",
            "large replay (512 units)",
        )


def test_freeze_large_corpus_copies_exact_response_file_workload(tmp_path: Path):
    generated = tmp_path / "generated"
    large = generated / "large"
    large.mkdir(parents=True)
    for index in range(513):
        (large / f"object-{index:03}.o").write_bytes(str(index).encode())

    corpus = tmp_path / "corpus"
    manifest_path = freeze_large_corpus(generated, corpus, tmp_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    assert manifest["configuration"] == "large replay (512 units)"
    assert manifest["expected_exit_code"] == 0
    assert len(manifest["objects"]) == 513
    assert len(list((corpus / "objs").glob("*.o"))) == 513


def test_freeze_large_corpus_rejects_incomplete_workload(tmp_path: Path):
    generated = tmp_path / "generated"
    large = generated / "large"
    large.mkdir(parents=True)
    (large / "main.o").write_bytes(b"main")

    with pytest.raises(WindowsCiError, match="expected 513"):
        freeze_large_corpus(generated, tmp_path / "corpus", tmp_path)


def test_freeze_large_corpus_refuses_to_reset_runner_temp(tmp_path: Path):
    generated = tmp_path / "generated"
    large = generated / "large"
    large.mkdir(parents=True)
    for index in range(513):
        (large / f"object-{index:03}.o").write_bytes(b"object")

    with pytest.raises(WindowsCiError, match="refusing to reset"):
        freeze_large_corpus(generated, tmp_path, tmp_path)


def test_msvc_linker_uses_visual_studio_tools_instead_of_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    tools = tmp_path / "MSVC" / "14.44"
    expected = tools / "bin" / "HostX64" / "x64" / "link.exe"
    expected.parent.mkdir(parents=True)
    expected.write_bytes(b"msvc")

    git_bin = tmp_path / "Git" / "usr" / "bin"
    git_bin.mkdir(parents=True)
    (git_bin / "link.exe").write_bytes(b"gnu")
    monkeypatch.setenv("PATH", str(git_bin))
    monkeypatch.setenv("VCToolsInstallDir", str(tools))

    assert _msvc_linker() == expected
    env = _msvc_path_env()
    assert env["PATH"].split(os.pathsep)[0] == str(expected.parent)
    assert env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] == str(expected)
