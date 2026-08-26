import os
from pathlib import Path

import pytest

from ci import windows_ci
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


def test_self_host_accepts_the_coff_bridge_version_marker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    commands: list[list[str]] = []

    monkeypatch.setattr(windows_ci, "_workspace", lambda: tmp_path)
    monkeypatch.setattr(windows_ci, "_require_file", lambda path, _label: path)
    monkeypatch.setattr(windows_ci, "_cargo", lambda *args: list(args))

    def run(args, **_kwargs):
        commands.append(list(args))
        return "LLD 22.1.2\n" if args[-1] == "--version" else ""

    monkeypatch.setattr(windows_ci, "_run", run)

    windows_ci.self_host()

    assert commands == [
        ["build", "-p", "reld", "--bin", "reld"],
        [str(tmp_path / "target" / "debug" / "reld.exe"), "--version"],
    ]
