"""Exercise every shipped reld link mode on the current host.

The GitHub Actions workflow is intentionally a thin tool installer and invokes this file with
``uv run``.  All mode selection, fixture construction, compiler/linker invocation, routing-log
validation, executable validation, and reporting live here so the pipeline is testable as Python.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
MODE_NAMES = ("fast", "thin-lto", "full-lto")
LLVM_BITCODE_MAGICS = (b"BC\xc0\xde", b"\xde\xc0\x17\x0b")
ROUTE_PATTERN = re.compile(r"reld: engine=(?P<engine>\S+) " r"\((?P<kind>native|bridge), reason=(?P<reason>[^)]+)\)")


class PipelineError(RuntimeError):
    """A linker-mode contract was not satisfied."""


@dataclass(frozen=True)
class Mode:
    name: str
    clang_flags: tuple[str, ...]
    expects_bitcode: bool


@dataclass(frozen=True)
class Host:
    name: str
    linker_binary: str
    executable_suffix: str
    object_suffix: str

    def expected_engine(self, mode: Mode) -> tuple[str, str]:
        if self.name == "linux":
            return ("reld", "native") if mode.name == "fast" else ("lld", "bridge")
        if self.name == "windows":
            return "lld-link", "bridge"
        return "ld64.lld", "bridge"


@dataclass(frozen=True)
class ModeResult:
    platform: str
    mode: str
    engine: str
    route_kind: str
    reason: str
    executable: str


MODES = {
    "fast": Mode("fast", (), False),
    "thin-lto": Mode("thin-lto", ("-flto=thin",), True),
    "full-lto": Mode("full-lto", ("-flto=full",), True),
}

HOSTS = {
    "linux": Host("linux", "reld", "", ".o"),
    "windows": Host("windows", "reld-link.exe", ".exe", ".obj"),
    "macos": Host("macos", "reld", "", ".o"),
}


def detect_platform(value: str = "auto") -> Host:
    if value != "auto":
        try:
            return HOSTS[value]
        except KeyError as error:
            raise PipelineError(f"unsupported platform {value!r}") from error
    if sys.platform.startswith("linux"):
        return HOSTS["linux"]
    if sys.platform == "win32":
        return HOSTS["windows"]
    if sys.platform == "darwin":
        return HOSTS["macos"]
    raise PipelineError(f"unsupported host platform {sys.platform!r}")


def select_modes(names: Iterable[str]) -> list[Mode]:
    requested = list(names)
    if not requested:
        requested = list(MODE_NAMES)
    unknown = [name for name in requested if name not in MODES]
    if unknown:
        raise PipelineError(f"unknown link mode(s): {', '.join(unknown)}")
    if len(set(requested)) != len(requested):
        raise PipelineError("link modes must not be repeated")
    return [MODES[name] for name in requested]


def command_text(argv: Sequence[os.PathLike[str] | str]) -> str:
    return subprocess.list2cmdline([os.fspath(arg) for arg in argv])


def run_checked(
    argv: Sequence[os.PathLike[str] | str],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [os.fspath(arg) for arg in argv]
    print(f"+ {command_text(command)}", flush=True)
    result = subprocess.run(
        command,
        cwd=cwd,
        env=None if env is None else dict(env),
        text=True,
        capture_output=True,
        errors="replace",
        check=False,
    )
    if result.stdout:
        print(result.stdout, end="" if result.stdout.endswith("\n") else "\n")
    if result.stderr:
        print(result.stderr, end="" if result.stderr.endswith("\n") else "\n", file=sys.stderr)
    if result.returncode != 0:
        raise PipelineError(f"command exited with {result.returncode}: {command_text(command)}")
    return result


def cargo_argv(value: str) -> list[str]:
    argv = shlex.split(value, posix=os.name != "nt")
    if not argv:
        raise PipelineError("cargo command must not be empty")
    return argv


def executable_from_cargo_output(output: str, binary_name: str) -> Path | None:
    for line in output.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == binary_name and message.get("executable"):
            return Path(message["executable"])
    return None


def build_linker(
    host: Host,
    *,
    repo_root: Path,
    cargo: Sequence[str],
    env: Mapping[str, str],
) -> Path:
    binary_name = Path(host.linker_binary).stem
    built = run_checked(
        [
            *cargo,
            "build",
            "--locked",
            "--package",
            "reld",
            "--bin",
            binary_name,
            "--message-format=json-render-diagnostics",
        ],
        cwd=repo_root,
        env=env,
    )
    linker = executable_from_cargo_output(built.stdout, binary_name)
    if linker is None:
        raise PipelineError(f"cargo did not report an executable for {binary_name}")
    if not linker.is_file():
        raise PipelineError(f"built linker not found at {linker}")
    return linker.resolve()


def compile_command(
    compiler: str,
    source: Path,
    output: Path,
    mode: Mode,
) -> list[str]:
    return [compiler, "-c", source, "-o", output, "-O2", *mode.clang_flags]


def link_command(
    compiler: str,
    objects: Sequence[Path],
    output: Path,
    linker: Path,
    mode: Mode,
    host: Host,
) -> list[str]:
    linker_selector = "reld-link" if host.name == "windows" else str(linker)
    return [
        compiler,
        *objects,
        "-o",
        output,
        f"-fuse-ld={linker_selector}",
        *mode.clang_flags,
    ]


def is_llvm_bitcode(path: Path) -> bool:
    return path.read_bytes()[:4] in LLVM_BITCODE_MAGICS


def validate_object(path: Path, mode: Mode) -> None:
    actual = is_llvm_bitcode(path)
    if actual != mode.expects_bitcode:
        expected = "LLVM bitcode" if mode.expects_bitcode else "a native object"
        raise PipelineError(f"{path} is not {expected}")


def parse_route(output: str, host: Host, mode: Mode) -> tuple[str, str, str]:
    routes = [match.groupdict() for match in ROUTE_PATTERN.finditer(output)]
    if not routes:
        raise PipelineError(f"{mode.name}: reld emitted no routing decision")
    expected_engine, expected_kind = host.expected_engine(mode)
    matching = [route for route in routes if route["engine"] == expected_engine and route["kind"] == expected_kind]
    if not matching:
        rendered = ", ".join(f"{r['engine']}/{r['kind']}" for r in routes)
        raise PipelineError(f"{host.name}/{mode.name}: expected {expected_engine}/{expected_kind}, got {rendered}")
    engines = {(route["engine"], route["kind"]) for route in routes}
    if engines != {(expected_engine, expected_kind)}:
        raise PipelineError(f"{host.name}/{mode.name}: inconsistent routes: {sorted(engines)}")
    route = matching[0]
    return route["engine"], route["kind"], route["reason"]


def write_fixture(directory: Path) -> tuple[Path, Path]:
    value = directory / "value.c"
    main = directory / "main.c"
    value.write_text(
        "int linker_mode_value(void) { return 42; }\n",
        encoding="utf-8",
    )
    main.write_text(
        '#include <stdio.h>\nint linker_mode_value(void);\nint main(void) {\n  if (linker_mode_value() != 42) return 1;\n  puts("reld-link-mode-ok");\n  return 0;\n}\n',
        encoding="utf-8",
    )
    return main, value


def exercise_mode(
    host: Host,
    mode: Mode,
    *,
    compiler: str,
    linker: Path,
    root: Path,
    env: Mapping[str, str],
) -> ModeResult:
    mode_dir = root / mode.name
    mode_dir.mkdir(parents=True)
    command_env = dict(env)
    command_env["RELD_LOG_ENGINE"] = "1"
    if host.name == "windows":
        command_env["PATH"] = os.pathsep.join([str(linker.parent), command_env.get("PATH", "")])
    sources = write_fixture(mode_dir)
    objects: list[Path] = []
    for source in sources:
        output = mode_dir / f"{source.stem}{host.object_suffix}"
        run_checked(
            compile_command(compiler, source, output, mode),
            cwd=mode_dir,
            env=command_env,
        )
        validate_object(output, mode)
        objects.append(output)

    executable = mode_dir / f"fixture-{mode.name}{host.executable_suffix}"
    linked = run_checked(
        link_command(compiler, objects, executable, linker, mode, host),
        cwd=mode_dir,
        env=command_env,
    )
    engine, route_kind, reason = parse_route(linked.stdout + linked.stderr, host, mode)
    executed = run_checked([executable], cwd=mode_dir, env=command_env)
    if "reld-link-mode-ok" not in executed.stdout:
        raise PipelineError(f"{host.name}/{mode.name}: executable output marker missing")
    return ModeResult(
        platform=host.name,
        mode=mode.name,
        engine=engine,
        route_kind=route_kind,
        reason=reason,
        executable=str(executable),
    )


def write_report(path: Path, results: Sequence[ModeResult]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema": 1,
        "results": [asdict(result) for result in results],
    }
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def publish_summary(results: Sequence[ModeResult]) -> None:
    rows = [
        "### Reld linker modes",
        "",
        "| Platform | Mode | Engine | Route | Reason |",
        "|---|---|---|---|---|",
    ]
    rows.extend(f"| {result.platform} | {result.mode} | {result.engine} | {result.route_kind} | `{result.reason}` |" for result in results)
    summary = "\n".join(rows) + "\n"
    print(summary)
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(summary)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=("auto", *HOSTS), default="auto")
    parser.add_argument("--mode", action="append", choices=MODE_NAMES, default=[])
    parser.add_argument("--compiler", default=os.environ.get("CLANG", "clang"))
    parser.add_argument("--cargo-command", default=os.environ.get("CARGO_COMMAND", "cargo"))
    parser.add_argument("--reld", type=Path, help="use a prebuilt linker and skip cargo build")
    parser.add_argument("--report", type=Path, default=Path("linker-modes.json"))
    args = parser.parse_args(argv)

    host = detect_platform(args.platform)
    modes = select_modes(args.mode)
    env = dict(os.environ)
    linker = (
        args.reld.resolve()
        if args.reld
        else build_linker(
            host,
            repo_root=REPO_ROOT,
            cargo=cargo_argv(args.cargo_command),
            env=env,
        )
    )
    if not linker.is_file():
        raise PipelineError(f"linker not found at {linker}")

    with tempfile.TemporaryDirectory(prefix="reld-linker-modes-") as temporary:
        results = [
            exercise_mode(
                host,
                mode,
                compiler=args.compiler,
                linker=linker,
                root=Path(temporary),
                env=env,
            )
            for mode in modes
        ]
    write_report(args.report, results)
    publish_summary(results)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PipelineError as error:
        print(f"linker-modes: {error}", file=sys.stderr)
        raise SystemExit(1) from error
