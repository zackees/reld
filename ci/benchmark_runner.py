"""Benchmark one idiomatic Rust project's final link across three LTO configurations.

Cargo compiles ``ci/e2e/sqlite-bridge`` exactly once per configuration and asks rustc to print
the target-native final linker invocation.  Warmups and timed trials replay only that captured
invocation with the selected linker; compilation and executable validation stay outside timing.
"""

from __future__ import annotations

import argparse
import ast
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path, PureWindowsPath
from typing import TextIO


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = REPO_ROOT / "ci" / "e2e" / "sqlite-bridge" / "Cargo.toml"


class BenchmarkError(RuntimeError):
    """The benchmark setup, replay, or executable oracle failed."""


@dataclass(frozen=True)
class Configuration:
    label: str
    profile: str


CONFIGURATIONS = (
    Configuration("no-LTO", "linkbench-no-lto"),
    Configuration("ThinLTO", "linkbench-thin-lto"),
    Configuration("full-LTO", "linkbench-full-lto"),
)


@dataclass(frozen=True)
class LinkCommand:
    executable: str
    arguments: tuple[str, ...]
    output: Path
    windows: bool


@dataclass(frozen=True)
class Linker:
    label: str
    path: Path | None = None


_QUOTED_ARGUMENT = re.compile(r'"(?:\\.|[^"\\])*"')
_LINK_DRIVER_NAMES = {
    "cc",
    "clang",
    "clang++",
    "gcc",
    "g++",
    "ld",
    "link.exe",
    "lld-link",
    "lld-link.exe",
    "rust-lld",
}


def cargo_capture_command(*, cargo: str, manifest: Path, profile: str, target_dir: Path) -> list[str]:
    if not cargo or any(character.isspace() for character in cargo):
        raise BenchmarkError("CARGO_COMMAND must name one executable")
    return [
        cargo,
        "rustc",
        "--locked",
        "--manifest-path",
        str(manifest),
        "--package",
        "app",
        "--profile",
        profile,
        "--target-dir",
        str(target_dir),
        "--",
        "-C",
        "save-temps=yes",
        "--print",
        "link-args",
    ]


def benchmark_environment() -> dict[str, str]:
    if sys.platform == "win32":
        # Imported only on the platform that owns the MSVC environment contract.
        from ci.windows_ci import _msvc_path_env

        return _msvc_path_env()
    return os.environ.copy()


def _output_from_arguments(arguments: tuple[str, ...], *, windows: bool) -> Path | None:
    if windows:
        for argument in arguments:
            if argument.upper().startswith("/OUT:"):
                return Path(argument[5:])
        return None
    for index, argument in enumerate(arguments[:-1]):
        if argument == "-o":
            return Path(arguments[index + 1])
    return None


def parse_print_link_args(output: str, *, windows: bool) -> LinkCommand:
    """Parse rustc's Rust-escaped ``--print link-args`` command line."""
    for line in reversed(output.splitlines()):
        # Rust's Command debug form may prefix target-specific assignments and, on Apple targets,
        # `env -u ...` removals. Decode all quoted tokens, then locate the actual driver by name.
        tokens = _QUOTED_ARGUMENT.findall(line)
        if len(tokens) < 2:
            continue
        try:
            decoded = [ast.literal_eval(token) for token in tokens]
        except (SyntaxError, ValueError):
            continue
        if not all(isinstance(argument, str) for argument in decoded):
            continue
        for index, executable in enumerate(decoded[:-1]):
            name = (PureWindowsPath(executable).name if windows else Path(executable).name).lower()
            if name not in _LINK_DRIVER_NAMES:
                continue
            arguments = tuple(decoded[index + 1 :])
            linked_output = _output_from_arguments(arguments, windows=windows)
            if linked_output is not None:
                return LinkCommand(executable, arguments, linked_output, windows)
    raise BenchmarkError("rustc --print link-args emitted no complete final linker command")


def replace_output(command: LinkCommand, output: Path) -> LinkCommand:
    arguments = list(command.arguments)
    if command.windows:
        for index, argument in enumerate(arguments):
            if argument.upper().startswith("/OUT:"):
                arguments[index] = f"/OUT:{output}"
                return LinkCommand(command.executable, tuple(arguments), output, command.windows)
    else:
        for index, argument in enumerate(arguments[:-1]):
            if argument == "-o":
                arguments[index + 1] = str(output)
                return LinkCommand(command.executable, tuple(arguments), output, command.windows)
    raise BenchmarkError("captured final link command has no output argument")


def _windows_response_file(arguments: tuple[str, ...]) -> str:
    # MSVC-family linkers accept one CommandLineToArgvW-quoted argument per line.  Preparing this
    # once keeps both response-file I/O and Windows' process command-line limit outside timing.
    return "\n".join(subprocess.list2cmdline([argument]) for argument in arguments) + "\n"


def replay_command(
    captured: LinkCommand,
    *,
    linker: str,
    linker_path: Path | None,
    output: Path,
    response_file: Path,
    driver_linker_dir: Path | None = None,
) -> tuple[list[str], str | None]:
    command = replace_output(captured, output)
    if command.windows:
        executable = command.executable if linker == "link.exe" else None
        if linker_path is not None:
            executable = str(linker_path)
        if executable is None:
            raise BenchmarkError(f"no executable resolved for Windows linker {linker!r}")
        response = _windows_response_file(command.arguments)
        return [executable, f"@{response_file}"], response

    if linker_path is None:
        raise BenchmarkError(f"no executable resolved for Unix linker {linker!r}")
    # Remove rustc's own linker override first. Linux GCC accepts only a fixed set of -fuse-ld
    # names, so its caller supplies a prebuilt -B directory containing `ld`. Apple Clang accepts
    # absolute -fuse-ld paths, which preserve flavor-bearing ld64.lld/ld64.reld basenames.
    arguments = [argument for argument in command.arguments if not argument.startswith(("-fuse-ld=", "-B"))]
    if driver_linker_dir is not None:
        arguments.append(f"-B{driver_linker_dir}")
    elif linker != "ld":
        arguments.append(f"-fuse-ld={linker_path}")
    return [command.executable, *arguments], None


def _required_tool(*names: str) -> Path:
    for name in names:
        resolved = shutil.which(name)
        if resolved:
            # Preserve flavor-bearing names such as ld64.lld and lld-link.exe. LLD and reld
            # dispatch from argv[0], so resolving a symlink to a generic `lld`/`reld` is wrong.
            return Path(resolved).absolute()
    raise BenchmarkError(f"required linker executable not found: {' or '.join(names)}")


def linkers_for_target(target: str, reld: Path) -> tuple[Linker, ...]:
    normalized = target.lower()
    if "windows" in normalized or "msvc" in normalized:
        return (
            Linker("link.exe"),
            Linker("lld", _required_tool("lld-link.exe", "lld-link")),
            Linker("reld", reld.absolute()),
        )
    if "darwin" in normalized or "macos" in normalized:
        return (
            Linker("ld", _required_tool("ld")),
            Linker("ld64.lld", _required_tool("ld64.lld")),
            Linker("reld", reld.absolute()),
        )
    if "linux" in normalized:
        return (
            Linker("bfd", _required_tool("ld.bfd", "ld")),
            Linker("lld", _required_tool("ld.lld")),
            Linker("mold", _required_tool("ld.mold", "mold")),
            Linker("wild", _required_tool("ld.wild", "wild")),
            Linker("reld", reld.absolute()),
        )
    raise BenchmarkError(f"unsupported benchmark target {target!r}")


def capture_final_link(
    configuration: Configuration,
    *,
    cargo: str,
    manifest: Path,
    target_dir: Path,
    environment: dict[str, str],
    log: TextIO,
) -> LinkCommand:
    command = cargo_capture_command(
        cargo=cargo,
        manifest=manifest,
        profile=configuration.profile,
        target_dir=target_dir,
    )
    print(f"capturing {configuration.label}: {' '.join(command)}", file=log, flush=True)
    completed = subprocess.run(
        command,
        cwd=manifest.parent,
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if completed.returncode:
        raise BenchmarkError(f"{configuration.label} compilation/capture failed:\n{completed.stderr}")
    try:
        return parse_print_link_args(completed.stdout, windows=sys.platform == "win32")
    except BenchmarkError as error:
        raise BenchmarkError(f"{error}\nrustc stdout:\n{completed.stdout}\nrustc stderr:\n{completed.stderr}") from error


def _run_link(command: list[str], *, cwd: Path, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


def _validate_executable(output: Path, *, cwd: Path, environment: dict[str, str]) -> None:
    if not output.is_file() or output.stat().st_size == 0:
        raise BenchmarkError(f"linked output is missing or empty: {output}")
    completed = subprocess.run(
        [output],
        cwd=cwd,
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if completed.returncode or "OK" not in completed.stdout:
        raise BenchmarkError(f"linked output failed its OK oracle ({completed.returncode}):\n" f"{completed.stdout}{completed.stderr}")


def benchmark_replay(
    captured: LinkCommand,
    linker: Linker,
    *,
    output_dir: Path,
    cwd: Path,
    environment: dict[str, str],
    warmup: int,
    trials: int,
    use_driver_shim: bool,
) -> float:
    output_dir.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if captured.windows else ""
    output = output_dir / f"app-{linker.label.replace('.', '-')}{suffix}"
    response_file = output.with_suffix(output.suffix + ".rsp")
    driver_linker_dir = None
    if not captured.windows and use_driver_shim:
        if linker.path is None:
            raise BenchmarkError(f"no executable resolved for Unix linker {linker.label!r}")
        driver_linker_dir = output_dir / f"driver-{linker.label.replace('.', '-')}"
        driver_linker_dir.mkdir(parents=True, exist_ok=True)
        linker_shim = driver_linker_dir / "ld"
        linker_shim.unlink(missing_ok=True)
        linker_shim.symlink_to(linker.path)
    command, response = replay_command(
        captured,
        linker=linker.label,
        linker_path=linker.path,
        output=output,
        response_file=response_file,
        driver_linker_dir=driver_linker_dir,
    )
    if response is not None:
        response_file.write_text(response, encoding="utf-8", newline="\n")

    for _ in range(warmup):
        output.unlink(missing_ok=True)
        completed = _run_link(command, cwd=cwd, environment=environment)
        if completed.returncode:
            raise BenchmarkError(f"{linker.label} warmup link failed:\n{completed.stdout}{completed.stderr}")

    samples: list[float] = []
    for _ in range(trials):
        output.unlink(missing_ok=True)
        started = time.perf_counter()
        completed = _run_link(command, cwd=cwd, environment=environment)
        elapsed = time.perf_counter() - started
        if completed.returncode:
            raise BenchmarkError(f"{linker.label} timed link failed:\n{completed.stdout}{completed.stderr}")
        samples.append(elapsed)

    _validate_executable(output, cwd=cwd, environment=environment)
    return statistics.median(samples)


def run_benchmark(
    *,
    target: str,
    reld: Path,
    manifest: Path,
    workdir: Path,
    target_dir: Path,
    cargo: str,
    trials: int,
    warmup: int,
    log: TextIO,
) -> str:
    environment = benchmark_environment()
    linkers = linkers_for_target(target, reld)
    use_driver_shim = "linux" in target.lower()
    results: list[tuple[str, list[float]]] = []
    for configuration in CONFIGURATIONS:
        captured = capture_final_link(
            configuration,
            cargo=cargo,
            manifest=manifest,
            target_dir=target_dir,
            environment=environment,
            log=log,
        )
        timings = [
            benchmark_replay(
                captured,
                linker,
                output_dir=workdir / configuration.profile,
                cwd=manifest.parent,
                environment=environment,
                warmup=warmup,
                trials=trials,
                use_driver_shim=use_driver_shim,
            )
            for linker in linkers
        ]
        results.append((configuration.label, timings))

    lines = [
        f"## Link Benchmark: {target}",
        "",
        "| Configuration | " + " | ".join(linker.label for linker in linkers) + " |",
        "|:--------------|" + "|".join("----:" for _ in linkers) + "|",
    ]
    lines.extend(f"| {label} | " + " | ".join(f"{timing:.4f}" for timing in timings) + " |" for label, timings in results)
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--reld", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--workdir", type=Path, default=Path("benchmark-output/replay"))
    parser.add_argument("--target-dir", type=Path, default=REPO_ROOT / "target" / "link-benchmark")
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=1)
    args = parser.parse_args(argv)

    if not args.reld.is_file():
        parser.error(f"reld driver not found: {args.reld}")
    if not args.manifest.is_file():
        parser.error(f"benchmark project manifest not found: {args.manifest}")
    if args.trials < 1 or args.warmup < 0:
        parser.error("trials must be positive and warmup must be non-negative")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with args.output.open("w", encoding="utf-8", newline="") as log:
            table = run_benchmark(
                target=args.target,
                reld=args.reld.absolute(),
                manifest=args.manifest.resolve(),
                workdir=args.workdir.resolve(),
                target_dir=args.target_dir.resolve(),
                cargo=os.environ.get("CARGO_COMMAND", "cargo"),
                trials=args.trials,
                warmup=args.warmup,
                log=log,
            )
            log.write(table)
    except BenchmarkError as error:
        message = f"benchmark failed: {error}\n"
        with args.output.open("a", encoding="utf-8", newline="") as log:
            log.write(message)
        sys.stderr.write(args.output.read_text(encoding="utf-8", errors="replace"))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
