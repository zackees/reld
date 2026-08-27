"""Benchmark one significant Rust project's final link across three LTO configurations.

Cargo compiles ``ci/e2e/link-workload`` exactly once per configuration and asks rustc to print
the target-native final linker invocation.  Warmups and timed trials replay only that captured
invocation with the selected linker; compilation and executable validation stay outside timing.
Each linker's fixed process startup is measured separately and never subtracted from final links.
"""

from __future__ import annotations

import argparse
import ast
import json
import os
import platform
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
DEFAULT_MANIFEST = REPO_ROOT / "ci" / "e2e" / "link-workload" / "Cargo.toml"
BENCHMARK_PACKAGE = "linkbench-app"
ISSUE_74_BASELINE_SHA = "be6a8cd032ea6dc788978264c860fe761d5090b6"
MIN_REFERENCE_LINK_SECONDS = 0.500
MAX_STARTUP_FRACTION = 0.10
MIN_OUTPUT_MODE_IMPROVEMENT = 0.10


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
    driver_arguments: tuple[str, ...] = ()


@dataclass(frozen=True)
class ExecutableOracle:
    stdout: str
    stderr: str


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


def cargo_capture_command(
    *,
    cargo: str,
    manifest: Path,
    profile: str,
    target_dir: Path,
    linker: str | None = None,
) -> list[str]:
    if not cargo or any(character.isspace() for character in cargo):
        raise BenchmarkError("CARGO_COMMAND must name one executable")
    command = [
        cargo,
        "rustc",
        "--locked",
        "--manifest-path",
        str(manifest),
        "--package",
        BENCHMARK_PACKAGE,
        "--profile",
        profile,
        "--target-dir",
        str(target_dir),
        "--",
    ]
    if linker is not None:
        command.extend(("-C", f"linker={linker}"))
    command.extend(
        (
            "-C",
            "save-temps=yes",
            "--print",
            "link-args",
        )
    )
    return command


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
            Linker("reld", reld.absolute(), ("-Wl,--engine=reld",)),
        )
    raise BenchmarkError(f"unsupported benchmark target {target!r}")


def _target_family(target: str) -> str:
    normalized = target.lower()
    if "windows" in normalized or "msvc" in normalized:
        return "windows"
    if "darwin" in normalized or "macos" in normalized:
        return "macos"
    if "linux" in normalized:
        return "linux"
    raise BenchmarkError(f"unsupported benchmark target {target!r}")


def reference_linker_for_target(target: str) -> str:
    """Return the fastest portable native reference available for this object format."""
    return {"linux": "wild", "macos": "ld64.lld", "windows": "lld"}[_target_family(target)]


def reld_output_mode_linkers(reld: Path, baseline_reld: Path, thread_count: int) -> tuple[Linker, ...]:
    """Return HEAD modes plus the fixed pre-issue baseline for the Linux diagnostic."""
    common_arguments = ("-Wl,--engine=reld", f"-Wl,--threads={thread_count}")
    return (
        Linker("baseline", baseline_reld.absolute(), common_arguments + ("-Wl,--mmap-output-file",)),
        Linker("default", reld.absolute(), common_arguments),
        Linker("mmap", reld.absolute(), common_arguments + ("-Wl,--mmap-output-file",)),
        Linker("buffer", reld.absolute(), common_arguments + ("-Wl,--no-mmap-output-file",)),
    )


def startup_probe_command(target: str, linker: Linker, captured: LinkCommand | None = None) -> list[str]:
    """Build a target-correct, no-link-work command that still loads the concrete linker."""
    executable = str(linker.path) if linker.path is not None else None
    if executable is None and captured is not None and linker.label == "link.exe":
        executable = captured.executable
    if executable is None:
        raise BenchmarkError(f"no executable resolved for startup probe {linker.label!r}")
    flag = {"linux": "--version", "macos": "-v", "windows": "/?"}[_target_family(target)]
    return [executable, flag]


def benchmark_startup(
    target: str,
    linker: Linker,
    *,
    captured: LinkCommand,
    environment: dict[str, str],
    warmup: int,
    trials: int,
) -> float:
    """Measure process/front-door startup without performing a final link."""
    command = startup_probe_command(target, linker, captured)
    for _ in range(warmup):
        completed = _run_link(command, cwd=Path.cwd(), environment=environment)
        if completed.returncode < 0:
            raise BenchmarkError(f"{linker.label} startup probe terminated by signal {-completed.returncode}")

    samples: list[float] = []
    for _ in range(trials):
        started = time.perf_counter()
        completed = _run_link(command, cwd=Path.cwd(), environment=environment)
        samples.append(time.perf_counter() - started)
        if completed.returncode < 0:
            raise BenchmarkError(f"{linker.label} startup probe terminated by signal {-completed.returncode}")
    return statistics.median(samples)


def assert_significant_workload(
    target: str,
    startup_seconds: dict[str, float],
    final_links: dict[str, dict[str, float]],
) -> None:
    """Reject workloads where fixed startup can materially determine the chart ordering."""
    if not startup_seconds:
        raise BenchmarkError("startup timings are missing")
    largest_startup = max(startup_seconds.values())
    reference = reference_linker_for_target(target)
    failures: list[str] = []
    for configuration in CONFIGURATIONS:
        reference_seconds = final_links.get(configuration.label, {}).get(reference)
        if reference_seconds is None or reference_seconds <= 0:
            failures.append(f"{configuration.label}: missing {reference} reference timing")
            continue
        fraction = largest_startup / reference_seconds
        if fraction > MAX_STARTUP_FRACTION:
            failures.append(f"{configuration.label}: largest startup {largest_startup:.4f}s is {fraction:.1%} of {reference} final link {reference_seconds:.4f}s")
    if failures:
        calibration = f" (calibration target: {MIN_REFERENCE_LINK_SECONDS:.3f}s reference link)"
        raise BenchmarkError("startup-dominated workload" + calibration + ":\n" + "\n".join(failures))


def assert_output_mode_improvement(diagnostic_report: dict[str, object]) -> None:
    """Require HEAD to beat the exact pre-issue baseline binary in every Linux row."""
    if diagnostic_report.get("status") != "measured":
        return
    configurations = diagnostic_report.get("configurations")
    if not isinstance(configurations, dict):
        raise BenchmarkError("output-mode diagnostics omitted configurations")

    failures: list[str] = []
    for configuration in CONFIGURATIONS:
        configuration_report = configurations.get(configuration.label)
        modes = configuration_report.get("modes") if isinstance(configuration_report, dict) else None
        default = modes.get("default") if isinstance(modes, dict) else None
        baseline = modes.get("baseline") if isinstance(modes, dict) else None
        mmap = modes.get("mmap") if isinstance(modes, dict) else None
        default_seconds = default.get("median_seconds") if isinstance(default, dict) else None
        baseline_seconds = baseline.get("median_seconds") if isinstance(baseline, dict) else None
        mmap_seconds = mmap.get("median_seconds") if isinstance(mmap, dict) else None
        if not isinstance(default_seconds, int | float) or not isinstance(baseline_seconds, int | float) or baseline_seconds <= 0:
            failures.append(f"{configuration.label}: missing paired HEAD/baseline medians")
            continue
        improvement = 1.0 - (default_seconds / baseline_seconds)
        configuration_report["head_vs_baseline_improvement_fraction"] = improvement
        if isinstance(mmap_seconds, int | float) and mmap_seconds > 0:
            configuration_report["default_vs_mmap_improvement_fraction"] = 1.0 - (default_seconds / mmap_seconds)
        if improvement + 1e-12 < MIN_OUTPUT_MODE_IMPROVEMENT:
            failures.append(f"{configuration.label}: HEAD improved {improvement:.1%} over baseline ({default_seconds:.4f}s vs {baseline_seconds:.4f}s; required {MIN_OUTPUT_MODE_IMPROVEMENT:.0%})")
    if failures:
        raise BenchmarkError("large-output optimization missed its paired performance gate:\n" + "\n".join(failures))


def capture_final_link(
    configuration: Configuration,
    *,
    cargo: str,
    manifest: Path,
    target_dir: Path,
    environment: dict[str, str],
    log: TextIO,
    linker: str | None = None,
    timeout_seconds: float | None = None,
) -> LinkCommand:
    command = cargo_capture_command(
        cargo=cargo,
        manifest=manifest,
        profile=configuration.profile,
        target_dir=target_dir,
        linker=linker,
    )
    print(f"capturing {configuration.label}: {' '.join(command)}", file=log, flush=True)
    try:
        completed = subprocess.run(
            command,
            cwd=manifest.parent,
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        raise BenchmarkError(f"{configuration.label} compilation/capture exceeded {timeout_seconds:.0f}s") from error
    if completed.returncode:
        raise BenchmarkError(f"{configuration.label} compilation/capture failed:\n{completed.stderr}")
    try:
        return parse_print_link_args(completed.stdout, windows=sys.platform == "win32")
    except BenchmarkError as error:
        raise BenchmarkError(f"{error}\nrustc stdout:\n{completed.stdout}\nrustc stderr:\n{completed.stderr}") from error


def prune_capture_artifacts(command: LinkCommand, *, profile_dir: Path, log: TextIO) -> None:
    """Remove only compiler outputs that the captured native link does not reference."""
    argument_text = "\n".join(command.arguments).replace("\\", "/").lower()
    removed_files = 0
    removed_bytes = 0
    for path in profile_dir.rglob("*"):
        if not path.is_file() or path.suffix.lower() not in {".bc", ".d", ".rmeta"}:
            continue
        normalized = str(path.absolute()).replace("\\", "/").lower()
        if normalized in argument_text:
            continue
        removed_bytes += path.stat().st_size
        path.unlink()
        removed_files += 1

    # Cargo already proved this exact command can produce an executable. Replays replace the
    # output path and validate every measured linker, so retaining Cargo's copy only wastes disk.
    command.output.unlink(missing_ok=True)
    print(
        f"pruned {removed_files} unreferenced capture files ({removed_bytes / (1024**2):.1f} MiB)",
        file=log,
        flush=True,
    )


def release_capture_artifacts(*, profile_dir: Path, target_dir: Path, log: TextIO) -> None:
    """Release one configuration after every linker has replayed its captured link."""
    allowed_profiles = {configuration.profile for configuration in CONFIGURATIONS}
    resolved_target = target_dir.resolve()
    is_junction = getattr(profile_dir, "is_junction", lambda: False)
    if profile_dir.name not in allowed_profiles or profile_dir.parent.resolve() != resolved_target or profile_dir.is_symlink() or is_junction() or profile_dir.resolve().parent != resolved_target:
        raise BenchmarkError(f"refusing to release unsafe capture profile: {profile_dir}")
    if profile_dir.exists():
        shutil.rmtree(profile_dir)
    print(f"released capture profile {profile_dir.name}", file=log, flush=True)


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


def _run_executable(output: Path, *, cwd: Path, environment: dict[str, str], timeout_seconds: float | None = None) -> subprocess.CompletedProcess[str]:
    if not output.is_file() or output.stat().st_size == 0:
        raise BenchmarkError(f"linked output is missing or empty: {output}")
    return subprocess.run(
        [output],
        cwd=cwd,
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
        timeout=timeout_seconds,
    )


def capture_executable_oracle(
    output: Path,
    *,
    cwd: Path,
    environment: dict[str, str],
    timeout_seconds: float | None = None,
) -> ExecutableOracle:
    """Capture exact behavior from Cargo's target-native reference executable."""
    try:
        completed = _run_executable(output, cwd=cwd, environment=environment, timeout_seconds=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        raise BenchmarkError(f"captured reference exceeded {timeout_seconds:.0f}s: {output}") from error
    if completed.returncode or not completed.stdout.startswith("OK "):
        raise BenchmarkError(f"captured reference failed its OK oracle ({completed.returncode}):\n{completed.stdout}{completed.stderr}")
    return ExecutableOracle(stdout=completed.stdout, stderr=completed.stderr)


def _validate_executable(
    output: Path,
    *,
    oracle: ExecutableOracle,
    cwd: Path,
    environment: dict[str, str],
) -> None:
    completed = _run_executable(output, cwd=cwd, environment=environment)
    if completed.returncode:
        raise BenchmarkError(f"linked output failed its executable oracle ({completed.returncode}):\n{completed.stdout}{completed.stderr}")
    if completed.stdout != oracle.stdout or completed.stderr != oracle.stderr:
        raise BenchmarkError(
            "linked output differed from Cargo's reference executable:\n"
            f"expected stdout: {oracle.stdout!r}\nactual stdout: {completed.stdout!r}\n"
            f"expected stderr: {oracle.stderr!r}\nactual stderr: {completed.stderr!r}"
        )


def _discard_verified_output(output: Path) -> Path | None:
    """Discard one verified output, quarantining a transiently locked Windows executable."""
    try:
        output.unlink()
    except PermissionError:
        if sys.platform != "win32":
            raise
        # Windows Defender or the loader can retain a handle briefly after process exit. Mirror
        # `clud trash` semantics: quarantine the exact artifact once, without delete retries or
        # guessed process termination. The Actions workspace itself is ephemeral.
        quarantine = output.with_name(f".{output.name}.trash-{time.time_ns()}")
        try:
            output.replace(quarantine)
        except PermissionError:
            # Every invocation has a unique output path, so even a non-renamable quarantined file
            # cannot block the next replay. The hosted runner reclaims the workspace after the job.
            return None
        else:
            return quarantine
    return None


def _reclaim_quarantines(quarantines: list[Path]) -> None:
    """Reclaim quarantines after a linker's timed trials, before another linker runs."""
    creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0) | getattr(subprocess, "DETACHED_PROCESS", 0)
    helpers = [
        subprocess.Popen(
            [sys.executable, "-m", "ci.quarantine_cleanup", str(quarantine)],
            cwd=REPO_ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            close_fds=True,
            creationflags=creationflags,
        )
        for quarantine in quarantines
    ]
    for helper in helpers:
        helper.wait()


def benchmark_replay(
    captured: LinkCommand,
    linker: Linker,
    *,
    oracle: ExecutableOracle,
    output_dir: Path,
    cwd: Path,
    environment: dict[str, str],
    warmup: int,
    trials: int,
    use_driver_shim: bool,
    sample_sink: list[float] | None = None,
    output_size_sink: list[int] | None = None,
) -> float:
    output_dir.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if captured.windows else ""
    output_stem = f"app-{linker.label.replace('.', '-')}"
    driver_linker_dir = None
    if not captured.windows and use_driver_shim:
        if linker.path is None:
            raise BenchmarkError(f"no executable resolved for Unix linker {linker.label!r}")
        driver_linker_dir = output_dir / f"driver-{linker.label.replace('.', '-')}"
        driver_linker_dir.mkdir(parents=True, exist_ok=True)
        linker_shim = driver_linker_dir / "ld"
        linker_shim.unlink(missing_ok=True)
        linker_shim.symlink_to(linker.path)

    def prepare_replay(output: Path) -> list[str]:
        response_file = output.with_suffix(output.suffix + ".rsp")
        command, response = replay_command(
            captured,
            linker=linker.label,
            linker_path=linker.path,
            output=output,
            response_file=response_file,
            driver_linker_dir=driver_linker_dir,
        )
        command.extend(linker.driver_arguments)
        if response is not None:
            response_file.write_text(response, encoding="utf-8", newline="\n")
        return command

    quarantines: list[Path] = []
    samples: list[float] = []
    try:
        for index in range(warmup):
            output = output_dir / f"{output_stem}-warmup-{index}{suffix}"
            command = prepare_replay(output)
            completed = _run_link(command, cwd=cwd, environment=environment)
            if completed.returncode:
                raise BenchmarkError(f"{linker.label} warmup link failed:\n{completed.stdout}{completed.stderr}")
            _validate_executable(output, oracle=oracle, cwd=cwd, environment=environment)
            if quarantine := _discard_verified_output(output):
                quarantines.append(quarantine)

        for index in range(trials):
            output = output_dir / f"{output_stem}-trial-{index}{suffix}"
            command = prepare_replay(output)
            started = time.perf_counter()
            completed = _run_link(command, cwd=cwd, environment=environment)
            elapsed = time.perf_counter() - started
            if completed.returncode:
                raise BenchmarkError(f"{linker.label} timed link failed:\n{completed.stdout}{completed.stderr}")
            samples.append(elapsed)
            # The oracle runs after the timer stops, once for every produced executable. A bad
            # early trial therefore cannot enter the median and then be hidden by an overwrite.
            _validate_executable(output, oracle=oracle, cwd=cwd, environment=environment)
            if output_size_sink is not None:
                output_size_sink.append(output.stat().st_size)
            if quarantine := _discard_verified_output(output):
                quarantines.append(quarantine)
    finally:
        # A cleaner that wakes during a later trial would contaminate that measurement with I/O.
        # Start and join all one-shot helpers only after this linker's timed samples are complete.
        _reclaim_quarantines(quarantines)

    if sample_sink is not None:
        sample_sink.extend(samples)
    return statistics.median(samples)


def _rotated(items: tuple[Linker, ...], offset: int) -> tuple[Linker, ...]:
    if not items:
        return ()
    split = offset % len(items)
    return items[split:] + items[:split]


def benchmark_linkers_round_robin(
    captured: LinkCommand,
    linkers: tuple[Linker, ...],
    *,
    oracle: ExecutableOracle,
    output_dir: Path,
    cwd: Path,
    environment: dict[str, str],
    warmup: int,
    trials: int,
    use_driver_shim: bool,
) -> tuple[dict[str, float], dict[str, list[float]], dict[str, list[int]], list[list[str]]]:
    """Replay linkers in rotating order so cache/writeback position cannot pick a winner."""
    if not linkers:
        raise BenchmarkError("at least one linker is required")

    samples = {linker.label: [] for linker in linkers}
    output_sizes = {linker.label: [] for linker in linkers}
    orders: list[list[str]] = []
    for round_index in range(warmup + trials):
        order = _rotated(linkers, round_index)
        if round_index >= warmup:
            orders.append([linker.label for linker in order])
        for linker in order:
            one_sample: list[float] = []
            one_size: list[int] = []
            benchmark_replay(
                captured,
                linker,
                oracle=oracle,
                # Each round must use a fresh /OUT path. A verified Windows executable can remain
                # locked long enough that neither unlink nor quarantine succeeds; reusing the
                # previous round's path would then make link.exe fail with LNK1104.
                output_dir=output_dir / linker.label.replace("/", "-") / f"round-{round_index}",
                cwd=cwd,
                environment=environment,
                warmup=0,
                trials=1,
                use_driver_shim=use_driver_shim,
                sample_sink=one_sample,
                output_size_sink=one_size,
            )
            if round_index >= warmup:
                samples[linker.label].extend(one_sample)
                output_sizes[linker.label].extend(one_size)

    medians = {label: statistics.median(values) for label, values in samples.items()}
    return medians, samples, output_sizes, orders


def _sample_summary(samples: list[float]) -> dict[str, float | list[float]]:
    median = statistics.median(samples)
    deviations = [abs(sample - median) for sample in samples]
    return {
        "samples_seconds": samples,
        "median_seconds": median,
        "median_absolute_deviation_seconds": statistics.median(deviations),
        "min_seconds": min(samples),
        "max_seconds": max(samples),
    }


def _linux_filesystem_type(path: Path, environment: dict[str, str]) -> str:
    completed = subprocess.run(
        ["findmnt", "--noheadings", "--output", "FSTYPE", "--target", str(path)],
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if completed.returncode:
        raise BenchmarkError(f"unable to identify benchmark filesystem:\n{completed.stderr}")
    filesystem = completed.stdout.strip()
    if not filesystem:
        raise BenchmarkError("findmnt reported an empty benchmark filesystem type")
    return filesystem


_PHASE_NAMES = (
    "Create output file",
    "Wait for output file creation",
    "Compute build ID",
    "Write data to file",
    "Flush and unmap output file",
)


def _parse_phase_timings(output: str, *, require_creation: bool = True) -> dict[str, float]:
    phases: dict[str, float] = {}
    for line in output.splitlines():
        for name in _PHASE_NAMES:
            match = re.search(rf"([0-9]+(?:\.[0-9]+)?)\s+{re.escape(name)}\s*$", line)
            if match:
                phases[name] = float(match.group(1)) / 1000.0
    required = {"Compute build ID", "Write data to file", "Flush and unmap output file"}
    if require_creation:
        required.add("Create output file")
    missing = sorted(required - phases.keys())
    if missing:
        raise BenchmarkError(f"reld phase trace omitted required phases: {', '.join(missing)}\n{output}")
    return phases


def capture_reld_phase_trace(
    captured: LinkCommand,
    linker: Linker,
    *,
    oracle: ExecutableOracle,
    output_dir: Path,
    cwd: Path,
    environment: dict[str, str],
    require_creation: bool = True,
) -> tuple[dict[str, float], int, str]:
    """Run one untimed, validated reld link with phase instrumentation enabled."""
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / "phase-trace"
    driver_dir = output_dir / "driver"
    driver_dir.mkdir(parents=True, exist_ok=True)
    if linker.path is None:
        raise BenchmarkError("no executable resolved for reld phase trace")
    shim = driver_dir / "ld"
    shim.unlink(missing_ok=True)
    shim.symlink_to(linker.path)
    command, response = replay_command(
        captured,
        linker=linker.label,
        linker_path=linker.path,
        output=output,
        response_file=output.with_suffix(".rsp"),
        driver_linker_dir=driver_dir,
    )
    if response is not None:
        raise BenchmarkError("Linux phase trace unexpectedly required a response file")
    command.extend(linker.driver_arguments)
    command.extend(("-Wl,--time", "-Wl,--no-fork"))
    completed = _run_link(command, cwd=cwd, environment=environment)
    if completed.returncode:
        raise BenchmarkError(f"reld phase trace failed:\n{completed.stdout}{completed.stderr}")
    _validate_executable(output, oracle=oracle, cwd=cwd, environment=environment)
    output_size = output.stat().st_size
    combined_output = completed.stdout + completed.stderr
    phases = _parse_phase_timings(combined_output, require_creation=require_creation)
    _discard_verified_output(output)
    return phases, output_size, combined_output


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
    output_mode_report: Path | None = None,
    baseline_reld: Path | None = None,
) -> str:
    environment = benchmark_environment()
    linkers = linkers_for_target(target, reld)
    use_driver_shim = "linux" in target.lower()
    startup_seconds: dict[str, float] | None = None
    final_links: dict[str, dict[str, float]] = {}
    diagnostic_report: dict[str, object] | None = None
    if output_mode_report is not None:
        diagnostic_report = {
            "schema_version": 1,
            "target": target,
            "status": "measured" if use_driver_shim else "not-applicable",
            "metadata": {},
            "configurations": {},
        }
        if use_driver_shim:
            if baseline_reld is None or not baseline_reld.is_file():
                raise BenchmarkError("Linux output-mode diagnostics require the exact baseline reld binary")
            workdir.mkdir(parents=True, exist_ok=True)
            thread_count = os.cpu_count() or 1
            diagnostic_report["metadata"] = {
                "runner_os": platform.platform(),
                "kernel": platform.release(),
                "machine": platform.machine(),
                "filesystem": _linux_filesystem_type(workdir, environment),
                "thread_count": thread_count,
                "output_mode": "explicit per contender",
                "timing_scope": "captured final native link only",
                "baseline_sha": ISSUE_74_BASELINE_SHA,
            }
    for configuration in CONFIGURATIONS:
        captured = capture_final_link(
            configuration,
            cargo=cargo,
            manifest=manifest,
            target_dir=target_dir,
            environment=environment,
            log=log,
            # GCC's collect2 injects a passive LTO plugin even when rustc has already emitted
            # native objects. That makes the conservative reld router bridge an otherwise native
            # final link. Clang is provisioned on Linux and replays the same target-native inputs
            # without manufacturing a plugin request.
            linker="clang" if use_driver_shim else None,
        )
        oracle = capture_executable_oracle(
            captured.output,
            cwd=manifest.parent,
            environment=environment,
        )
        print(
            f"captured executable oracle for {configuration.label}: stdout={oracle.stdout.strip()!r}, stderr_bytes={len(oracle.stderr.encode('utf-8'))}",
            file=log,
            flush=True,
        )
        prune_capture_artifacts(
            captured,
            profile_dir=target_dir / configuration.profile,
            log=log,
        )
        if startup_seconds is None:
            startup_seconds = {
                linker.label: benchmark_startup(
                    target,
                    linker,
                    captured=captured,
                    environment=environment,
                    warmup=warmup,
                    trials=trials,
                )
                for linker in linkers
            }
        try:
            medians, _samples, _sizes, _orders = benchmark_linkers_round_robin(
                captured,
                linkers,
                oracle=oracle,
                output_dir=workdir / configuration.profile / "published",
                cwd=manifest.parent,
                environment=environment,
                warmup=warmup,
                trials=trials,
                use_driver_shim=use_driver_shim,
            )
            final_links[configuration.label] = medians

            if diagnostic_report is not None and use_driver_shim:
                metadata = diagnostic_report["metadata"]
                assert isinstance(metadata, dict)
                thread_count = metadata["thread_count"]
                assert baseline_reld is not None
                modes = reld_output_mode_linkers(reld, baseline_reld, thread_count)
                diagnostic_trials = max(trials, len(modes))
                diagnostic_trials = diagnostic_trials + (-diagnostic_trials % len(modes))
                _, mode_samples, mode_sizes, mode_orders = benchmark_linkers_round_robin(
                    captured,
                    modes,
                    oracle=oracle,
                    output_dir=workdir / configuration.profile / "output-modes",
                    cwd=manifest.parent,
                    environment=environment,
                    warmup=warmup,
                    trials=diagnostic_trials,
                    use_driver_shim=True,
                )
                configuration_report: dict[str, object] = {
                    "trial_orders": mode_orders,
                    "trials_per_mode": diagnostic_trials,
                    "modes": {},
                }
                mode_reports = configuration_report["modes"]
                assert isinstance(mode_reports, dict)
                for mode in modes:
                    sizes = mode_sizes[mode.label]
                    if not sizes or min(sizes) < 240 * 1024 * 1024:
                        raise BenchmarkError(f"{configuration.label}/{mode.label} output is not approximately 256 MiB: {sizes}")
                    phases, phase_output_size, phase_trace = capture_reld_phase_trace(
                        captured,
                        mode,
                        oracle=oracle,
                        output_dir=workdir / configuration.profile / "phase-traces" / mode.label,
                        cwd=manifest.parent,
                        environment=environment,
                        require_creation=mode.label != "baseline",
                    )
                    mode_reports[mode.label] = {
                        **_sample_summary(mode_samples[mode.label]),
                        "output_sizes_bytes": sizes,
                        "phase_output_size_bytes": phase_output_size,
                        "phase_seconds": phases,
                        "phase_trace": phase_trace,
                        "phase_schema": "legacy-pre-creation-instrumentation" if mode.label == "baseline" else "current",
                    }
                configurations = diagnostic_report["configurations"]
                assert isinstance(configurations, dict)
                configurations[configuration.label] = configuration_report
        finally:
            # Only one configuration's retained native-link inputs live at a time. Compilation
            # and any Rust LTO preparation still happen once, before this configuration's timed
            # loop; removing its profile afterward cannot affect the already-recorded samples.
            release_capture_artifacts(
                profile_dir=target_dir / configuration.profile,
                target_dir=target_dir,
                log=log,
            )

    if startup_seconds is None:  # CONFIGURATIONS is intentionally non-empty; keep typing honest.
        raise BenchmarkError("startup timings are missing")

    lines = [
        f"## Link Benchmark: {target}",
        "",
        "| Configuration | " + " | ".join(linker.label for linker in linkers) + " |",
        "|:--------------|" + "|".join("----:" for _ in linkers) + "|",
    ]
    lines.extend(f"| {configuration.label} | " + " | ".join(f"{final_links[configuration.label][linker.label]:.4f}" for linker in linkers) + " |" for configuration in CONFIGURATIONS)
    lines.extend(
        [
            "",
            f"## Linker Startup: {target}",
            "",
            "| Linker | Seconds |",
            "|:-------|--------:|",
            *(f"| {linker.label} | {startup_seconds[linker.label]:.4f} |" for linker in linkers),
            "",
            "<!-- Startup is reported raw and is never subtracted from final-link medians. -->",
        ]
    )
    table = "\n".join(lines) + "\n"
    # Preserve the raw evidence even when the significance gate rejects the workload. This makes
    # calibration diagnosable without subtracting startup or weakening the publication gate.
    log.write(table)
    log.flush()
    output_mode_error: BenchmarkError | None = None
    if output_mode_report is not None and diagnostic_report is not None:
        try:
            assert_output_mode_improvement(diagnostic_report)
        except BenchmarkError as error:
            output_mode_error = error
        output_mode_report.parent.mkdir(parents=True, exist_ok=True)
        output_mode_report.write_text(json.dumps(diagnostic_report, indent=2) + "\n", encoding="utf-8")
    if output_mode_error is not None:
        raise output_mode_error
    assert_significant_workload(target, startup_seconds, final_links)
    return table


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
    parser.add_argument("--output-mode-report", type=Path)
    parser.add_argument("--baseline-reld", type=Path)
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
            run_benchmark(
                target=args.target,
                reld=args.reld.absolute(),
                manifest=args.manifest.resolve(),
                workdir=args.workdir.resolve(),
                target_dir=args.target_dir.resolve(),
                cargo=os.environ.get("CARGO_COMMAND", "cargo"),
                trials=args.trials,
                warmup=args.warmup,
                log=log,
                output_mode_report=args.output_mode_report,
                baseline_reld=args.baseline_reld,
            )
            # ``run_benchmark`` writes the evidence before applying the significance gate.
    except BenchmarkError as error:
        message = f"benchmark failed: {error}\n"
        with args.output.open("a", encoding="utf-8", newline="") as log:
            log.write(message)
        sys.stderr.write(args.output.read_text(encoding="utf-8", errors="replace"))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
