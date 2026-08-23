import os
from pathlib import Path

import pytest

from ci.benchmark_runner import benchmark_command, benchmark_environment


def test_benchmark_command_runs_link_only_harness_with_reld_driver():
    command = benchmark_command(
        target="x86_64-pc-windows-msvc",
        reld=Path("target/release/reld-link.exe"),
        trials=5,
        warmup=1,
        cargo="cargo",
    )

    assert command[:8] == [
        "cargo",
        "run",
        "--release",
        "-p",
        "reld-testkit",
        "--bin",
        "reld-bench",
        "--",
    ]
    assert command[-8:] == [
        "--target",
        "x86_64-pc-windows-msvc",
        "--trials",
        "5",
        "--warmup",
        "1",
        "--reld",
        str(Path("target/release/reld-link.exe")),
    ]


def test_benchmark_command_rejects_shell_fragments():
    with pytest.raises(ValueError, match="must name one executable"):
        benchmark_command(
            target="x86_64-linux",
            reld=Path("target/release/ld.reld"),
            trials=1,
            warmup=0,
            cargo="cargo --locked",
        )


def test_non_windows_benchmark_environment_is_inherited(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr("ci.benchmark_runner.sys.platform", "linux")
    monkeypatch.setenv("RELD_BENCHMARK_TEST", "present")

    assert benchmark_environment()["RELD_BENCHMARK_TEST"] == "present"
    assert benchmark_environment() is not os.environ
