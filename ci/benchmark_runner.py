"""Run the native benchmark with platform-safe linker process resolution."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path


def benchmark_command(
    *, target: str, reld: Path, trials: int, warmup: int, cargo: str
) -> list[str]:
    if not cargo or any(character.isspace() for character in cargo):
        raise ValueError("CARGO_COMMAND must name one executable")
    return [
        cargo,
        "run",
        "--release",
        "-p",
        "reld-testkit",
        "--bin",
        "reld-bench",
        "--",
        "--target",
        target,
        "--trials",
        str(trials),
        "--warmup",
        str(warmup),
        "--reld",
        str(reld),
    ]


def benchmark_environment() -> dict[str, str]:
    if sys.platform == "win32":
        # Imported only on the platform that owns the MSVC environment contract.
        from ci.windows_ci import _msvc_path_env

        return _msvc_path_env()
    return os.environ.copy()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--reld", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=1)
    args = parser.parse_args(argv)

    if not args.reld.is_file():
        parser.error(f"reld driver not found: {args.reld}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    command = benchmark_command(
        target=args.target,
        reld=args.reld,
        trials=args.trials,
        warmup=args.warmup,
        cargo=os.environ.get("CARGO_COMMAND", "cargo"),
    )
    with args.output.open("w", encoding="utf-8", newline="") as output:
        completed = subprocess.run(
            command,
            check=False,
            env=benchmark_environment(),
            stdout=output,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    if completed.returncode:
        sys.stderr.write(args.output.read_text(encoding="utf-8", errors="replace"))
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
