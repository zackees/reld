"""Python entry points for Windows CI steps.

GitHub Actions invokes this module from Bash. Keeping the platform orchestration here makes the
Windows checks locally testable and avoids embedding a second scripting language in ``ci.yml``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Sequence

from ci import benchmark_coverage


WINDOWS_TARGET = "x86_64-pc-windows-msvc"


class WindowsCiError(RuntimeError):
    """A Windows CI contract was not satisfied."""


def _cargo(*args: str) -> list[str]:
    command = os.environ.get("CARGO_COMMAND", "cargo")
    if not command or any(character.isspace() for character in command):
        raise WindowsCiError("CARGO_COMMAND must name one executable")
    return [command, *args]


def _run(
    args: Sequence[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> str:
    completed = subprocess.run(
        list(args),
        cwd=cwd,
        env=env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    sys.stdout.write(completed.stdout)
    if completed.returncode:
        rendered = subprocess.list2cmdline(list(args))
        raise WindowsCiError(f"command exited with {completed.returncode}: {rendered}")
    return completed.stdout


def _run_logged(
    args: Sequence[str],
    log: Path,
    *,
    append: bool = False,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> str:
    rendered = subprocess.list2cmdline(list(args))
    chunks = []
    with log.open("a" if append else "w", encoding="utf-8", newline="") as handle:
        process = subprocess.Popen(
            list(args),
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        assert process.stdout is not None
        for line in process.stdout:
            sys.stdout.write(line)
            handle.write(line)
            chunks.append(line)
        return_code = process.wait()
    if return_code:
        raise WindowsCiError(f"command exited with {return_code}: {rendered}")
    return "".join(chunks)


def _required_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise WindowsCiError(f"required environment variable {name} is missing")
    return value


def _workspace() -> Path:
    return Path(_required_env("GITHUB_WORKSPACE")).resolve()


def _runner_temp() -> Path:
    return Path(_required_env("RUNNER_TEMP")).resolve()


def _reset_under(path: Path, root: Path) -> Path:
    resolved = path.resolve()
    root = root.resolve()
    if resolved == root or root not in resolved.parents:
        raise WindowsCiError(f"refusing to reset path outside runner temp: {resolved}")
    if resolved.exists():
        shutil.rmtree(resolved)
    resolved.mkdir(parents=True)
    return resolved


def _require_file(path: Path, description: str) -> Path:
    if not path.is_file():
        raise WindowsCiError(f"{description} not found at {path}")
    return path


def _capture_allow_failure(args: Sequence[str]) -> str:
    completed = subprocess.run(
        list(args),
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    sys.stdout.write(completed.stdout)
    return completed.stdout


def verify_msvc_linkers() -> None:
    link_output = _capture_allow_failure(["link.exe"])
    lld_output = _run(["lld-link", "--version"])
    expected_msvc = _required_env("MSVC_LINK_VERSION")
    expected_lld = _required_env("LLD_VERSION")
    if expected_msvc not in link_output:
        raise WindowsCiError("link.exe version drift")
    if expected_lld not in lld_output:
        raise WindowsCiError("lld-link version drift")

    link_lines = link_output.splitlines()[:2]
    lld_lines = lld_output.splitlines()[:1]
    Path("versions.txt").write_text("\n".join([*link_lines, *lld_lines]) + "\n")

    source = _runner_temp() / "smoke.c"
    source.write_text("int main(void) { return 0; }\n", encoding="utf-8")
    _run(["cl.exe", "/nologo", str(source), f"/Fe:{source.with_name('smoke-link.exe')}"])
    _run(
        [
            "clang-cl.exe",
            "/nologo",
            "-fuse-ld=lld",
            str(source),
            f"/Fe:{source.with_name('smoke-lld.exe')}",
        ]
    )


def install_benchmark_linkers() -> None:
    _run(["choco", "install", "llvm", "--no-progress", "--yes"])
    program_files = Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
    llvm_bin = program_files / "LLVM" / "bin"
    _require_file(llvm_bin / "clang.exe", "clang.exe")
    _require_file(llvm_bin / "lld-link.exe", "lld-link.exe")
    if shutil.which("link.exe") is None:
        raise WindowsCiError("link.exe is missing from PATH")
    github_path = Path(_required_env("GITHUB_PATH"))
    with github_path.open("a", encoding="utf-8", newline="") as handle:
        handle.write(str(llvm_bin) + "\n")


def native_tests() -> None:
    _run(_cargo("build", "--workspace", "--all-targets"))
    commands = [
        _cargo(
            "test",
            "-p",
            "reld-layout-schema",
            "-p",
            "reld-diff",
            "-p",
            "reld-testkit",
            "--all-targets",
        ),
        _cargo("test", "-p", "reld-core", "--lib"),
        _cargo("test", "-p", "reld", "--bins"),
        _cargo("test", "-p", "reld", "--test", "acceptance-policy"),
        _cargo("test", "-p", "reld", "--test", "acceptance", "--", "--list"),
    ]
    log = Path("platform-tests.log")
    for index, command in enumerate(commands):
        _run_logged(command, log, append=index > 0)


def sqlite_bridge() -> None:
    workspace = _workspace()
    reld = _require_file(workspace / "target" / "debug" / "reld-link.exe", "reld-link.exe")
    env = os.environ.copy()
    env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] = str(reld)
    output = _run(
        _cargo("run", "--quiet", "--bin", "app"),
        cwd=workspace / "ci" / "e2e" / "sqlite-bridge",
        env=env,
    )
    if "OK" not in output:
        raise WindowsCiError("sqlite-bridge e2e output did not contain OK marker")
    if "linked SQLite" not in output:
        raise WindowsCiError("sqlite-bridge e2e output did not contain 'linked SQLite' marker")


def require_measured_row(text: str, scenario: str) -> None:
    timing = r"[0-9]+\.[0-9]{4}"
    pattern = rf"^\| {re.escape(scenario)} \| {timing} \| {timing} \| {timing} \|$"
    normalized = text.replace("\r\n", "\n")
    if not re.search(pattern, normalized, flags=re.MULTILINE):
        raise WindowsCiError(f"{scenario} link.exe/lld/reld row must contain real timings")


def require_generated_table(text: str) -> None:
    required = ["## Link Benchmark:", "| link.exe |", "| lld |", "| reld |"]
    missing = [marker for marker in required if marker not in text]
    if missing:
        raise WindowsCiError(f"benchmark table missing marker(s): {', '.join(missing)}")
    require_measured_row(text, "large (512 units)")


def freeze_large_corpus(generated: Path, corpus: Path, runner_temp: Path) -> Path:
    objects = sorted((generated / "large").glob("*.o"), key=lambda path: path.name)
    if len(objects) != 513:
        raise WindowsCiError(f"expected 513 large-workload objects (512 units plus main), got {len(objects)}")

    corpus = _reset_under(corpus, runner_temp)
    object_destination = corpus / "objs"
    object_destination.mkdir()
    object_names = []
    for source in objects:
        shutil.copy2(source, object_destination / source.name)
        object_names.append(f"objs/{source.name}")

    manifest = {
        "schema_version": 1,
        "platform": WINDOWS_TARGET,
        "configuration": "large replay (512 units)",
        "cc": "clang",
        "objects": object_names,
        "extra_link_args": [],
        "output_name": "bench.exe",
        "expected_exit_code": 0,
    }
    manifest_path = corpus / "corpus.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return manifest_path


def _coverage(log: Path, *, canonical: bool) -> None:
    args = ["--input-log", str(log), "--target", WINDOWS_TARGET]
    if not canonical:
        args.append("--allow-noncanonical-scenarios")
    if benchmark_coverage.main(args):
        raise WindowsCiError(f"benchmark coverage failed for {log}")


def benchmark_smoke() -> None:
    workspace = _workspace()
    runner_temp = _runner_temp()
    _run(_cargo("build", "-p", "reld", "--bin", "reld-link"))
    reld = _require_file(workspace / "target" / "debug" / "reld-link.exe", "reld-link.exe")

    generated = _reset_under(runner_temp / "reld-bench-generated", runner_temp)
    generated_log = Path("benchmark-windows-msvc.md")
    generated_text = _run_logged(
        _cargo(
            "run",
            "--release",
            "-p",
            "reld-testkit",
            "--bin",
            "reld-bench",
            "--",
            "--cc",
            "clang",
            "--target",
            WINDOWS_TARGET,
            "--trials",
            "2",
            "--warmup",
            "1",
            "--reld",
            str(reld),
            "--workdir",
            str(generated),
        ),
        generated_log,
    )
    require_generated_table(generated_text)
    _coverage(generated_log, canonical=True)

    corpus = runner_temp / "reld-bench-large-replay-corpus"
    freeze_large_corpus(generated, corpus, runner_temp)
    replay_workdir = _reset_under(runner_temp / "reld-bench-large-replay-output", runner_temp)
    replay_log = Path("benchmark-windows-msvc-replay.md")
    replay_text = _run_logged(
        _cargo(
            "run",
            "--release",
            "-p",
            "reld-testkit",
            "--bin",
            "reld-bench",
            "--",
            "--replay-corpus",
            str(corpus),
            "--target",
            WINDOWS_TARGET,
            "--trials",
            "2",
            "--warmup",
            "1",
            "--reld",
            str(reld),
            "--workdir",
            str(replay_workdir),
        ),
        replay_log,
    )
    require_measured_row(replay_text, "large replay (512 units)")
    _coverage(replay_log, canonical=False)


def build_benchmark_driver() -> None:
    workspace = _workspace()
    _run(_cargo("build", "--release", "-p", "reld", "--bin", "reld-link"))
    driver = _require_file(workspace / "target" / "release" / "reld-link.exe", "reld-link.exe")
    print(f"Using COFF bridge driver: {driver}")


def self_host() -> None:
    workspace = _workspace()
    reld_link = _require_file(workspace / "target" / "debug" / "reld-link.exe", "reld-link.exe")
    env = os.environ.copy()
    env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] = str(reld_link)
    _run(_cargo("build", "-p", "reld", "--bin", "reld"), env=env)
    reld = _require_file(workspace / "target" / "debug" / "reld.exe", "reld.exe")
    output = _run([str(reld), "--version"])
    if "Reld" not in output:
        raise WindowsCiError("self-linked reld --version output missing 'Reld' marker")


COMMANDS = {
    "verify-msvc-linkers": verify_msvc_linkers,
    "install-benchmark-linkers": install_benchmark_linkers,
    "native-tests": native_tests,
    "sqlite-bridge": sqlite_bridge,
    "benchmark-smoke": benchmark_smoke,
    "build-benchmark-driver": build_benchmark_driver,
    "self-host": self_host,
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ci.windows_ci")
    parser.add_argument("command", choices=COMMANDS)
    args = parser.parse_args(argv)
    try:
        COMMANDS[args.command]()
    except WindowsCiError as error:
        print(f"windows CI failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
