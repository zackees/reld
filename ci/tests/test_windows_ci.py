import os
from pathlib import Path

import pytest

from ci import windows_ci
from ci.windows_ci import (
    PHASE1_ACCEPTANCE_LIST_FILTER,
    PHASE1_ARCHIVE_ENV,
    PHASE1_NATIVE_FILTER,
    WindowsCiError,
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


def test_native_tests_replays_the_cross_built_archive_without_compiling(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    archive = tmp_path / "phase1-tests.tar.zst"
    archive.write_bytes(b"archive")

    monkeypatch.setattr(windows_ci, "_msvc_path_env", lambda: {})
    monkeypatch.setattr(windows_ci, "_workspace", lambda: tmp_path)
    monkeypatch.setattr(windows_ci, "_cargo", lambda *args: list(args))
    monkeypatch.setenv(PHASE1_ARCHIVE_ENV, str(archive))
    monkeypatch.chdir(tmp_path)

    commands: list[list[str]] = []

    def run_logged(command, _log, **_kwargs):
        commands.append(list(command))
        return ""

    monkeypatch.setattr(windows_ci, "_run_logged", run_logged)

    windows_ci.native_tests()

    assert len(commands) == 2
    run_command, list_command = commands

    assert run_command[:6] == [
        "nextest",
        "run",
        "--archive-file",
        str(archive.resolve()),
        "--workspace-remap",
        str(tmp_path),
    ]
    assert "-E" in run_command
    assert run_command[run_command.index("-E") + 1] == PHASE1_NATIVE_FILTER

    assert list_command[:6] == [
        "nextest",
        "list",
        "--archive-file",
        str(archive.resolve()),
        "--workspace-remap",
        str(tmp_path),
    ]
    assert "-E" in list_command
    assert list_command[list_command.index("-E") + 1] == PHASE1_ACCEPTANCE_LIST_FILTER

    assert not any("build" in command for command in commands)


def test_native_tests_requires_the_phase1_archive_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(windows_ci, "_msvc_path_env", lambda: {})
    monkeypatch.setattr(windows_ci, "_workspace", lambda: tmp_path)
    monkeypatch.setattr(windows_ci, "_cargo", lambda *args: list(args))
    monkeypatch.delenv(PHASE1_ARCHIVE_ENV, raising=False)
    monkeypatch.chdir(tmp_path)

    with pytest.raises(WindowsCiError):
        windows_ci.native_tests()


def test_echoing_non_ascii_child_output_survives_a_cp1252_console(monkeypatch, tmp_path):
    # The windows-msvc leg died here (reld#130): nextest printed characters cp1252 cannot encode,
    # and echoing them to a cp1252 stdout raised after the child had already succeeded.
    import io
    import sys

    raw = io.BytesIO()
    console = io.TextIOWrapper(raw, encoding="cp1252", errors="strict")
    monkeypatch.setattr(sys, "stdout", console)
    monkeypatch.setattr(sys, "stderr", io.TextIOWrapper(io.BytesIO(), encoding="cp1252"))

    windows_ci._utf8_console()
    log = tmp_path / "out.log"
    windows_ci._run_logged(
        [sys.executable, "-c", "import sys; sys.stdout.buffer.write('PASS \\u2714 ok\\n'.encode())"],
        log,
    )
    console.flush()

    assert "✔" in log.read_text(encoding="utf-8")
    assert "✔".encode("utf-8") in raw.getvalue()
