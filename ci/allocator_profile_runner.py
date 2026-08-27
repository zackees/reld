"""Collect non-timing sampled-pprof and exact-DHAT evidence for issue #93."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

from ci.allocator_benchmark import (
    BenchmarkFailure,
    _prepare_command,
    _run_native,
    clean_benchmark_environment,
    cleanup_ephemeral_outputs,
)
from ci.benchmark_runner import (
    CONFIGURATIONS,
    DEFAULT_MANIFEST,
    BenchmarkError,
    capture_executable_oracle,
    capture_final_link,
    prune_capture_artifacts,
    release_capture_artifacts,
)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    digest.update(path.read_bytes())
    return digest.hexdigest()


def profile_mode_environment(base: dict[str, str], mode: str, output: Path) -> dict[str, str]:
    environment, contaminated = clean_benchmark_environment(base)
    if contaminated:
        raise BenchmarkFailure("profiler environment contamination: " + ", ".join(contaminated))
    environment.update({"MIMALLOC_PROF": "0", "MIMALLOC_PROF_ACTIVE": "0", "MIMALLOC_DHAT": "0"})
    if mode == "pprof":
        environment.update({"MIMALLOC_PROF": "1", "MIMALLOC_PROF_DUMP_AT_EXIT": str(output)})
    elif mode == "dhat":
        environment["RELD_DHAT_OUTPUT"] = str(output)
    elif mode != "default":
        raise BenchmarkFailure(f"unknown diagnostic allocator mode: {mode}")
    return environment


def profile_evidence(path: Path, *, label: str) -> dict[str, object]:
    if not path.is_file() or path.stat().st_size == 0:
        raise BenchmarkFailure(f"{label} emitted no profile")
    return {"path": str(path), "size_bytes": path.stat().st_size, "sha256": _sha256(path)}


def require_identical_artifacts(hashes: dict[str, str], *, configuration: str) -> None:
    if len(set(hashes.values())) != 1:
        raise BenchmarkFailure(f"diagnostic artifact identity failed for {configuration}: {hashes}")


def run(args: argparse.Namespace) -> dict[str, object]:
    if sys.platform != "linux":
        raise BenchmarkFailure("allocator profiles are supported only on Linux")
    environment, contaminated = clean_benchmark_environment(dict(os.environ))
    if contaminated:
        raise BenchmarkFailure("profiler environment contamination: " + ", ".join(contaminated))
    output_dir = args.output_dir.resolve()
    target_dir = args.target_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    modes = {"default": args.default, "pprof": args.pprof, "dhat": args.dhat}
    report: dict[str, object] = {
        "schema": 1,
        "diagnostic_only": True,
        "timing_evidence": False,
        "configurations": {},
    }
    log_path = output_dir / "capture.log"
    with log_path.open("w", encoding="utf-8") as log:
        for configuration in CONFIGURATIONS:
            print(f"allocator profiles: capturing {configuration.label}", flush=True)
            captured = capture_final_link(
                configuration,
                cargo="cargo",
                manifest=args.manifest.resolve(),
                target_dir=target_dir,
                environment=environment,
                log=log,
                linker="clang",
                timeout_seconds=args.capture_timeout,
            )
            oracle = capture_executable_oracle(
                captured.output,
                cwd=args.manifest.parent,
                environment=environment,
                timeout_seconds=args.native_timeout,
            )
            prune_capture_artifacts(captured, profile_dir=target_dir / configuration.profile, log=log)
            hashes: dict[str, str] = {}
            profiles: dict[str, dict[str, object]] = {}
            mode_environments: dict[str, dict[str, str]] = {}
            try:
                for mode, binary in modes.items():
                    mode_dir = output_dir / configuration.profile / mode
                    mode_dir.mkdir(parents=True, exist_ok=True)
                    output = mode_dir / "app"
                    command = _prepare_command(captured, binary, output, mode_dir / "driver")
                    expected_profile: Path | None = None
                    if mode == "pprof":
                        expected_profile = mode_dir / "heap.prof"
                    elif mode == "dhat":
                        expected_profile = mode_dir / "dhat-heap.json"
                    mode_environment = profile_mode_environment(
                        environment,
                        mode,
                        expected_profile or mode_dir / "unused-profile-output",
                    )
                    mode_environments[mode] = {name: mode_environment[name] for name in ("MIMALLOC_PROF", "MIMALLOC_PROF_ACTIVE", "MIMALLOC_DHAT")}
                    try:
                        completed = subprocess.run(
                            command,
                            cwd=args.manifest.parent,
                            env=mode_environment,
                            capture_output=True,
                            text=True,
                            check=False,
                            timeout=args.link_timeout,
                        )
                    except subprocess.TimeoutExpired as error:
                        raise BenchmarkFailure(f"{configuration.label}/{mode} profile link exceeded {args.link_timeout:.0f}s") from error
                    if completed.returncode:
                        raise BenchmarkFailure(f"{configuration.label}/{mode} profile link failed: {completed.stderr}")
                    _run_native(
                        output,
                        oracle.stdout,
                        oracle.stderr,
                        mode_environment,
                        args.manifest.parent,
                        timeout_seconds=args.native_timeout,
                    )
                    hashes[mode] = _sha256(output)
                    if expected_profile is not None:
                        profiles[mode] = {
                            **profile_evidence(expected_profile, label=f"{configuration.label}/{mode}"),
                            "environment": {name: mode_environment[name] for name in ("MIMALLOC_PROF", "MIMALLOC_PROF_ACTIVE", "MIMALLOC_DHAT")},
                        }
                    output.unlink()
                    (mode_dir / "driver" / "ld").unlink(missing_ok=True)
                    (mode_dir / "driver").rmdir()
                require_identical_artifacts(hashes, configuration=configuration.label)
                report["configurations"][configuration.label] = {
                    "artifact_sha256": hashes["default"],
                    "mode_artifact_sha256": hashes,
                    "native_oracle": {"exit_code": 0, "stdout": oracle.stdout, "stderr": oracle.stderr},
                    "profiles": profiles,
                    "mode_environments": mode_environments,
                }
            finally:
                release_capture_artifacts(
                    profile_dir=target_dir / configuration.profile,
                    target_dir=target_dir,
                    log=log,
                )
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--default", required=True, type=Path)
    parser.add_argument("--pprof", required=True, type=Path)
    parser.add_argument("--dhat", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--target-dir", type=Path, default=Path("target/allocator-profiles/workload"))
    parser.add_argument("--capture-timeout", type=float, default=1_800.0)
    parser.add_argument("--native-timeout", type=float, default=30.0)
    parser.add_argument("--link-timeout", type=float, default=300.0)
    args = parser.parse_args(argv)
    report: dict[str, object] | None = None
    failure: Exception | None = None
    try:
        report = run(args)
    except (BenchmarkFailure, BenchmarkError, OSError, subprocess.SubprocessError) as error:
        failure = error
    finally:
        cleanup_ephemeral_outputs(args.target_dir, args.output_dir)
    if failure is not None:
        resolved_output = args.output_dir.resolve()
        for pattern in ("heap.prof", "dhat-heap.json"):
            for path in resolved_output.glob(f"**/{pattern}"):
                if path.is_file() and path.resolve().is_relative_to(resolved_output):
                    path.unlink()
        sys.stderr.write(f"allocator profiling failed: {failure}\n")
        return 1
    assert report is not None
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
