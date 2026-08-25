"""Build the fixed issue #74 baseline linker in an isolated temporary worktree."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from ci.benchmark_runner import ISSUE_74_BASELINE_SHA, REPO_ROOT


def _run(command: list[str], *, cwd: Path) -> None:
    subprocess.run(command, cwd=cwd, check=True)


def build_baseline(output: Path, *, cargo: str, baseline_sha: str = ISSUE_74_BASELINE_SHA) -> None:
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary_root = Path(tempfile.mkdtemp(prefix="reld-issue74-baseline-", dir=os.environ.get("RUNNER_TEMP")))
    worktree = temporary_root / "source"
    target_dir = temporary_root / "target"
    registered = False
    try:
        _run(["git", "fetch", "--no-tags", "--depth=1", "origin", baseline_sha], cwd=REPO_ROOT)
        _run(["git", "worktree", "add", "--detach", str(worktree), baseline_sha], cwd=REPO_ROOT)
        registered = True
        _run(
            [
                cargo,
                "build",
                "--locked",
                "--release",
                "--package",
                "reld",
                "--bin",
                "reld",
                "--manifest-path",
                str(worktree / "Cargo.toml"),
                "--target-dir",
                str(target_dir),
            ],
            cwd=worktree,
        )
        binary_name = "reld.exe" if os.name == "nt" else "reld"
        shutil.copy2(target_dir / "release" / binary_name, output)
    finally:
        if registered:
            subprocess.run(["git", "worktree", "remove", "--force", str(worktree)], cwd=REPO_ROOT, check=False)
        shutil.rmtree(temporary_root, ignore_errors=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo", default=os.environ.get("CARGO_COMMAND", "cargo"))
    parser.add_argument("--baseline-sha", default=ISSUE_74_BASELINE_SHA)
    args = parser.parse_args(argv)
    build_baseline(args.output, cargo=args.cargo, baseline_sha=args.baseline_sha)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
