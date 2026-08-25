from pathlib import Path

import ci.benchmark_baseline as baseline_module
from ci.benchmark_runner import ISSUE_74_BASELINE_SHA


def test_baseline_builder_uses_isolated_exact_sha_worktree(tmp_path: Path, monkeypatch):
    commands: list[list[str]] = []

    def fake_run(command: list[str], *, cwd: Path) -> None:
        del cwd
        commands.append(command)
        if command[:3] == ["git", "worktree", "add"]:
            Path(command[-2]).mkdir(parents=True)
        if "--target-dir" in command:
            target_dir = Path(command[command.index("--target-dir") + 1])
            binary = target_dir / "release" / ("reld.exe" if baseline_module.os.name == "nt" else "reld")
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"baseline-binary")

    monkeypatch.setattr(baseline_module, "_run", fake_run)
    monkeypatch.setattr(baseline_module.subprocess, "run", lambda *args, **kwargs: None)
    monkeypatch.setenv("RUNNER_TEMP", str(tmp_path))
    output = tmp_path / "out" / "reld"

    baseline_module.build_baseline(output, cargo="cargo")

    assert output.read_bytes() == b"baseline-binary"
    assert commands[0] == ["git", "fetch", "--no-tags", "--depth=1", "origin", ISSUE_74_BASELINE_SHA]
    assert commands[1][-1] == ISSUE_74_BASELINE_SHA
    assert "--locked" in commands[2]
    assert "--release" in commands[2]
