import os
from pathlib import Path

import pytest

from ci.windows_ci import (
    _msvc_linker,
    _msvc_path_env,
)


def test_msvc_linker_uses_visual_studio_tools_instead_of_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
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
