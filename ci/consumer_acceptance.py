"""Build and execute pinned Rust, C, and C++ consumers with reld.

This is an on-host acceptance gate, not a cross-compilation smoke test. It
requires independent evidence that the requested reld binary was selected and
that every linked program's own behavioral tests pass.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence


REPO_ROOT = Path(__file__).resolve().parents[1]
CMAKE_LINK_RULES = REPO_ROOT / "ci" / "cmake" / "reld-link-rules.cmake"
CPP_FIXTURE = REPO_ROOT / "ci" / "e2e" / "cpp-name-mangling"
PCRE2_TEST_TOOLS = REPO_ROOT / "ci" / "pcre2-test-tools.ps1"

RUST_CRATE = "xsv"
RUST_CRATE_VERSION = "0.13.0"
PCRE2_REPOSITORY = "https://github.com/PCRE2Project/pcre2.git"
PCRE2_COMMIT = "f454e231fe5006dd7ff8f4693fd2b8eb94333429"


class AcceptanceError(RuntimeError):
    """A consumer did not prove that reld linked a working program."""


@dataclass(frozen=True)
class Host:
    name: str
    target: str
    executable_suffix: str
    rust_uses_compiler_wrapper: bool
    expected_engine: str
    expected_route_kind: str
    expected_route: str


HOSTS = {
    "linux": Host(
        name="linux",
        target="x86_64-unknown-linux-gnu",
        executable_suffix="",
        rust_uses_compiler_wrapper=True,
        expected_engine="reld",
        expected_route_kind="native",
        expected_route="reld: engine=reld (native,",
    ),
    "windows": Host(
        name="windows",
        target="x86_64-pc-windows-msvc",
        executable_suffix=".exe",
        rust_uses_compiler_wrapper=False,
        expected_engine="lld-link",
        expected_route_kind="bridge",
        expected_route="reld: engine=lld-link (bridge,",
    ),
    "macos": Host(
        name="macos",
        target="aarch64-apple-darwin",
        executable_suffix="",
        rust_uses_compiler_wrapper=True,
        expected_engine="ld64.lld",
        expected_route_kind="bridge",
        expected_route="reld: engine=ld64.lld (bridge,",
    ),
}


def detect_host(value: str) -> Host:
    if value != "auto":
        try:
            return HOSTS[value]
        except KeyError as error:
            raise AcceptanceError(f"unsupported platform {value!r}") from error
    if sys.platform.startswith("linux"):
        return HOSTS["linux"]
    if sys.platform == "win32":
        return HOSTS["windows"]
    if sys.platform == "darwin":
        return HOSTS["macos"]
    raise AcceptanceError(f"unsupported host {sys.platform!r}")


def command_text(command: Sequence[os.PathLike[str] | str]) -> str:
    return subprocess.list2cmdline([os.fspath(item) for item in command])


def run_checked(
    command: Sequence[os.PathLike[str] | str],
    *,
    cwd: Path,
    env: Mapping[str, str],
) -> subprocess.CompletedProcess[str]:
    rendered = command_text(command)
    print(f"+ {rendered}", flush=True)
    result = subprocess.run(
        [os.fspath(item) for item in command],
        cwd=cwd,
        env=dict(env),
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
        raise AcceptanceError(f"command exited with {result.returncode}: {rendered}")
    return result


def require_tool(name: str) -> str:
    resolved = shutil.which(name)
    if resolved is None:
        raise AcceptanceError(f"required tool {name!r} is not on PATH")
    # Keep the selected shim name. Rustup's cargo executable is a proxy whose
    # behavior depends on argv[0]; resolving it to rustup.exe changes the tool.
    return str(Path(resolved).absolute())


def require_route(output: str, host: Host, description: str) -> None:
    if host.expected_route not in output:
        raise AcceptanceError(
            f"{description} did not emit the expected reld route {host.expected_route!r}"
        )


def require_exact_output(
    result: subprocess.CompletedProcess[str],
    *,
    stdout: str,
    description: str,
) -> None:
    if result.stdout != stdout or result.stderr != "":
        raise AcceptanceError(
            f"{description} output mismatch: stdout={result.stdout!r}, stderr={result.stderr!r}"
        )


def rust_environment(
    host: Host,
    linker: Path,
    target_dir: Path,
    invocation_log: Path,
) -> dict[str, str]:
    env = os.environ.copy()
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    env.pop("RUSTFLAGS", None)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    key = "CARGO_TARGET_" + host.target.upper().replace("-", "_") + "_LINKER"
    rustflags_key = "CARGO_TARGET_" + host.target.upper().replace("-", "_") + "_RUSTFLAGS"
    if not host.rust_uses_compiler_wrapper:
        env[key] = str(linker)
        env.pop(rustflags_key, None)
    else:
        # Rust's direct linker flavors bypass the compiler driver, but a Rust consumer also needs
        # the driver to add the host CRT and SDK inputs. Keep that driver and select the exact
        # reld binary through clang's stable `-fuse-ld=` route; clang then hands reld raw linker
        # argv rather than compiler-driver flags such as `-m64` or `-Wl,...`.
        env[key] = "clang"
        fuse_ld_flag = f"-Clink-arg=-fuse-ld={linker}"
        env[rustflags_key] = fuse_ld_flag
        # Cargo links host build scripts separately from the requested target. On a native host
        # those scripts need the same compiler-wrapper route, so apply it at Cargo's
        # highest-precedence environment layer.
        env["CARGO_ENCODED_RUSTFLAGS"] = fuse_ld_flag
        env["RUSTFLAGS"] = fuse_ld_flag
    env["RELD_LOG_ENGINE"] = "1"
    env["RELD_INVOCATION_LOG"] = str(invocation_log)
    return env


def read_invocations(path: Path) -> list[dict[str, object]]:
    if not path.is_file():
        raise AcceptanceError(f"reld did not create its invocation log: {path}")
    records: list[dict[str, object]] = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            raise AcceptanceError(f"invalid invocation JSON at {path}:{line_number}") from error
        if not isinstance(record, dict):
            raise AcceptanceError(f"invocation record at {path}:{line_number} is not an object")
        records.append(record)
    if not records:
        raise AcceptanceError(f"reld invocation log is empty: {path}")
    return records


def record_output(record: Mapping[str, object]) -> Path | None:
    output = record.get("output")
    working_directory = record.get("working_directory")
    if not isinstance(output, str) or not isinstance(working_directory, str):
        return None
    path = Path(output)
    if not path.is_absolute():
        path = Path(working_directory) / path
    return path.resolve()


def require_logged_outputs(
    invocation_log: Path,
    host: Host,
    expected_outputs: Sequence[Path],
    description: str,
) -> None:
    records = read_invocations(invocation_log)
    successful_outputs = {
        record_output(record)
        for record in records
        if record.get("schema") == 1
        and record.get("status") == "success"
        and record.get("engine") == host.expected_engine
        and record.get("route_kind") == host.expected_route_kind
    }
    missing = [
        str(path.resolve())
        for path in expected_outputs
        if path.resolve() not in successful_outputs
    ]
    if missing:
        raise AcceptanceError(
            f"{description} has no successful reld invocation for: {', '.join(missing)}"
        )


def require_logged_rust_binary(invocation_log: Path, host: Host, target_dir: Path) -> Path:
    expected_parent = (target_dir / host.target / "release" / "deps").resolve()
    prefix = f"{RUST_CRATE}-"
    candidates = []
    for record in read_invocations(invocation_log):
        output = record_output(record)
        if (
            record.get("schema") == 1
            and record.get("status") == "success"
            and record.get("engine") == host.expected_engine
            and record.get("route_kind") == host.expected_route_kind
            and output is not None
            and output.parent == expected_parent
            and output.name.startswith(prefix)
            and output.name.endswith(host.executable_suffix)
        ):
            candidates.append(output)
    if len(candidates) != 1:
        raise AcceptanceError(
            f"cargo install {RUST_CRATE} logged {len(candidates)} matching final links; expected 1"
        )
    if not candidates[0].is_file():
        raise AcceptanceError(f"logged Rust output is missing: {candidates[0]}")
    return candidates[0]


def exercise_rust(host: Host, linker: Path, work_dir: Path) -> None:
    print(f"== Rust consumer: {RUST_CRATE} {RUST_CRATE_VERSION} ==", flush=True)
    install_root = work_dir / "rust-install"
    target_dir = work_dir / "rust-target"
    invocation_log = work_dir / "rust-invocations.jsonl"
    env = rust_environment(host, linker, target_dir, invocation_log)
    run_checked(
        [
            require_tool("cargo"),
            "install",
            RUST_CRATE,
            "--version",
            RUST_CRATE_VERSION,
            "--locked",
            "--root",
            install_root,
            "--target",
            host.target,
            "--target-dir",
            target_dir,
        ],
        cwd=REPO_ROOT,
        env=env,
    )

    require_logged_rust_binary(invocation_log, host, target_dir)

    sample = work_dir / "sample.csv"
    sample.write_text("name,value\nalpha,1\nbeta,2\n", encoding="utf-8")
    executable = install_root / "bin" / f"{RUST_CRATE}{host.executable_suffix}"
    if not executable.is_file():
        raise AcceptanceError(f"installed Rust executable is missing: {executable}")
    executed = run_checked([executable, "count", sample], cwd=work_dir, env=env)
    require_exact_output(executed, stdout="2\n", description="xsv count")


def exercise_logging_equivalence(
    host: Host,
    linker: Path,
    work_dir: Path,
    *,
    cc: str,
) -> None:
    print("== Logging equivalence: identical artifact with audit off/on ==", flush=True)
    fixture = work_dir / "logging-equivalence"
    fixture.mkdir()
    source = fixture / "main.c"
    object_file = fixture / ("main.obj" if host.name == "windows" else "main.o")
    executable = fixture / f"logging-equivalence{host.executable_suffix}"
    invocation_log = fixture / "invocations.jsonl"
    source.write_text("int main(void) { return 0; }\n", encoding="utf-8")

    env_without_log = cmake_environment(linker, host=host)
    selector = cmake_linker_selector(host, linker, env_without_log)
    compiler = require_tool(cc)
    run_checked([compiler, "-c", source, "-o", object_file], cwd=fixture, env=env_without_log)
    link_command = [compiler, object_file, "-o", executable, f"-fuse-ld={selector}"]
    if host.name == "windows":
        link_command.extend(["-Xlinker", "/Brepro"])
    elif host.name == "macos":
        link_command.append("-Wl,-no_uuid")
    else:
        link_command.append("-Wl,--build-id=sha1")

    first_link = run_checked(link_command, cwd=fixture, env=env_without_log)
    require_route(first_link.stdout + first_link.stderr, host, "logging-disabled link")
    baseline = executable.read_bytes()
    second_off_link = run_checked(link_command, cwd=fixture, env=env_without_log)
    require_route(second_off_link.stdout + second_off_link.stderr, host, "logging-disabled replay")
    if executable.read_bytes() != baseline:
        raise AcceptanceError("logging-disabled linker output is not self-deterministic")

    env_with_log = cmake_environment(linker, invocation_log, host=host)
    first_on_link = run_checked(link_command, cwd=fixture, env=env_with_log)
    require_route(first_on_link.stdout + first_on_link.stderr, host, "logging-enabled link")
    logged_baseline = executable.read_bytes()
    second_on_link = run_checked(link_command, cwd=fixture, env=env_with_log)
    require_route(second_on_link.stdout + second_on_link.stderr, host, "logging-enabled replay")
    require_logged_outputs(invocation_log, host, [executable], "logging-enabled link")
    if executable.read_bytes() != logged_baseline:
        raise AcceptanceError("logging-enabled linker output is not self-deterministic")
    if logged_baseline != baseline:
        raise AcceptanceError("RELD_INVOCATION_LOG changed the linked artifact bytes")

    executed = run_checked([executable], cwd=fixture, env=env_with_log)
    require_exact_output(executed, stdout="", description="logging-equivalence executable")


def checkout_pinned(repository: str, commit: str, destination: Path, env: Mapping[str, str]) -> None:
    destination.mkdir(parents=True)
    git = require_tool("git")
    run_checked([git, "init"], cwd=destination, env=env)
    run_checked([git, "remote", "add", "origin", repository], cwd=destination, env=env)
    run_checked([git, "fetch", "--depth", "1", "origin", commit], cwd=destination, env=env)
    run_checked([git, "checkout", "--detach", "FETCH_HEAD"], cwd=destination, env=env)
    actual = run_checked([git, "rev-parse", "HEAD"], cwd=destination, env=env).stdout.strip()
    if actual != commit:
        raise AcceptanceError(f"pinned checkout drifted: expected {commit}, got {actual}")


def prepare_pcre2_tests(host: Host, source: Path) -> None:
    if host.name != "windows":
        return
    script = source / "RunGrepTest.bat"
    contents = script.read_text(encoding="utf-8")
    helper = str(PCRE2_TEST_TOOLS)
    replacements = {
        "set printf=cscript //nologo printf.js": (
            f'set printf=powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass '
            f'-File "{helper}" printf'
        ),
        "set trnull=cscript //nologo trnull.js": (
            f'set trnull=powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass '
            f'-File "{helper}" trnull'
        ),
    }
    for original, replacement in replacements.items():
        if contents.count(original) != 1:
            raise AcceptanceError(f"pinned PCRE2 helper marker changed: {original}")
        contents = contents.replace(original, replacement)
    script.write_text(contents, encoding="utf-8", newline="\n")


def cmake_environment(
    linker: Path,
    invocation_log: Path | None = None,
    *,
    host: Host | None = None,
) -> dict[str, str]:
    env = os.environ.copy()
    if host is not None and host.name == "windows":
        # Git Bash exports a Unix locale name that the Windows CRT rejects.
        for variable in ("LANG", "LC_ALL", "LC_CTYPE"):
            env.pop(variable, None)
    env["RELD_LOG_ENGINE"] = "1"
    env["PATH"] = os.pathsep.join([str(linker.parent), env.get("PATH", "")])
    if invocation_log is not None:
        env["RELD_INVOCATION_LOG"] = str(invocation_log)
    else:
        env.pop("RELD_INVOCATION_LOG", None)
    return env


def cmake_linker_selector(host: Host, linker: Path, env: Mapping[str, str]) -> str:
    if host.name != "windows":
        return str(linker)
    resolved = shutil.which("reld-link", path=env.get("PATH"))
    if resolved is None or Path(resolved).resolve() != linker:
        raise AcceptanceError(
            f"{host.name} compiler cannot resolve the exact reld-link under test: {linker}"
        )
    return "reld-link"


def configure_and_build(
    *,
    host: Host,
    linker: Path,
    source_dir: Path,
    build_dir: Path,
    cc: str,
    cxx: str | None,
    definitions: Sequence[str],
    invocation_log: Path,
) -> str:
    env = cmake_environment(linker, invocation_log, host=host)
    selector = cmake_linker_selector(host, linker, env)
    env["RELD_LINKER_SELECTOR"] = selector
    command = [
        require_tool("cmake"),
        "-S",
        source_dir,
        "-B",
        build_dir,
        "-G",
        "Ninja",
        "-DCMAKE_BUILD_TYPE=Release",
        f"-DCMAKE_C_COMPILER={require_tool(cc)}",
        f"-DCMAKE_USER_MAKE_RULES_OVERRIDE={CMAKE_LINK_RULES}",
        *definitions,
    ]
    if cxx is not None:
        command.append(f"-DCMAKE_CXX_COMPILER={require_tool(cxx)}")
    run_checked(command, cwd=REPO_ROOT, env=env)
    built = run_checked(
        [require_tool("cmake"), "--build", build_dir, "--parallel", "4", "--verbose"],
        cwd=REPO_ROOT,
        env=env,
    )
    output = built.stdout + built.stderr
    require_route(output, host, f"CMake build of {source_dir.name}")
    return output


def ctest_count(build_dir: Path, env: Mapping[str, str]) -> int:
    listed = run_checked(
        [require_tool("ctest"), "--test-dir", build_dir, "--show-only=json-v1"],
        cwd=REPO_ROOT,
        env=env,
    )
    try:
        payload = json.loads(listed.stdout)
        return len(payload["tests"])
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise AcceptanceError(f"could not parse CTest inventory for {build_dir}") from error


def run_ctest(build_dir: Path, linker: Path, host: Host, *, minimum_tests: int) -> None:
    env = cmake_environment(linker, host=host)
    count = ctest_count(build_dir, env)
    if count < minimum_tests:
        raise AcceptanceError(
            f"{build_dir.name} registered {count} CTest tests; expected at least {minimum_tests}"
        )
    run_checked(
        [require_tool("ctest"), "--test-dir", build_dir, "--output-on-failure"],
        cwd=REPO_ROOT,
        env=env,
    )


def exercise_c(host: Host, linker: Path, work_dir: Path, *, cc: str) -> None:
    print(f"== C consumer: PCRE2 {PCRE2_COMMIT} ==", flush=True)
    source = work_dir / "pcre2"
    build = work_dir / "pcre2-build"
    invocation_log = work_dir / "pcre2-invocations.jsonl"
    env = cmake_environment(linker, host=host)
    checkout_pinned(PCRE2_REPOSITORY, PCRE2_COMMIT, source, env)
    prepare_pcre2_tests(host, source)
    definitions = [
        "-DBUILD_SHARED_LIBS=OFF",
        "-DPCRE2_BUILD_TESTS=ON",
        "-DPCRE2_SUPPORT_LIBBZ2=OFF",
        "-DPCRE2_SUPPORT_LIBZ=OFF",
        "-DPCRE2_SUPPORT_LIBREADLINE=OFF",
        "-DPCRE2_SUPPORT_LIBEDIT=OFF",
    ]
    if host.name == "windows":
        # External script callouts depend on optional Windows script engines.
        # PCRE2's complete non-fork callout suite still runs.
        definitions.append("-DPCRE2GREP_SUPPORT_CALLOUT_FORK=OFF")
    configure_and_build(
        host=host,
        linker=linker,
        source_dir=source,
        build_dir=build,
        cc=cc,
        cxx=None,
        definitions=definitions,
        invocation_log=invocation_log,
    )
    require_logged_outputs(
        invocation_log,
        host,
        [
            build / f"pcre2test{host.executable_suffix}",
            build / f"pcre2grep{host.executable_suffix}",
            build / f"pcre2posix_test{host.executable_suffix}",
        ],
        "PCRE2 build",
    )
    run_ctest(build, linker, host, minimum_tests=3)


def exercise_cpp(host: Host, linker: Path, work_dir: Path, *, cc: str, cxx: str) -> None:
    print("== C++ consumer: name-mangling fixture ==", flush=True)
    build = work_dir / "cpp-name-mangling-build"
    invocation_log = work_dir / "cpp-name-mangling-invocations.jsonl"
    configure_and_build(
        host=host,
        linker=linker,
        source_dir=CPP_FIXTURE,
        build_dir=build,
        cc=cc,
        cxx=cxx,
        definitions=("-DBUILD_SHARED_LIBS=OFF",),
        invocation_log=invocation_log,
    )
    require_logged_outputs(
        invocation_log,
        host,
        [build / f"reld_cpp_name_mangling{host.executable_suffix}"],
        "C++ name-mangling build",
    )
    executable = build / f"reld_cpp_name_mangling{host.executable_suffix}"
    executed = run_checked(
        [executable],
        cwd=build,
        env=cmake_environment(linker, host=host),
    )
    require_exact_output(
        executed,
        stdout="reld-cxx-name-mangling-ok\n",
        description="C++ name-mangling executable",
    )
    run_ctest(build, linker, host, minimum_tests=1)


def run_acceptance(
    *,
    host: Host,
    linker: Path,
    work_dir: Path,
    cc: str,
    cxx: str,
) -> None:
    linker = linker.resolve()
    if not linker.is_file():
        raise AcceptanceError(f"reld linker is missing: {linker}")
    if work_dir.exists() and any(work_dir.iterdir()):
        raise AcceptanceError(f"work directory must be empty: {work_dir}")
    work_dir.mkdir(parents=True, exist_ok=True)
    exercise_logging_equivalence(host, linker, work_dir, cc=cc)
    exercise_rust(host, linker, work_dir)
    exercise_c(host, linker, work_dir, cc=cc)
    exercise_cpp(host, linker, work_dir, cc=cc, cxx=cxx)
    print(f"PASS: reld linked working Rust, C, and C++ consumers on {host.name}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ci.consumer_acceptance")
    parser.add_argument("--platform", choices=("auto", *HOSTS), default="auto")
    parser.add_argument("--linker", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--cc", default="clang")
    parser.add_argument("--cxx", default="clang++")
    args = parser.parse_args(argv)
    try:
        run_acceptance(
            host=detect_host(args.platform),
            linker=args.linker,
            work_dir=args.work_dir.resolve(),
            cc=args.cc,
            cxx=args.cxx,
        )
    except AcceptanceError as error:
        print(f"consumer acceptance failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
