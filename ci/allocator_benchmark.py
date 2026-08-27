"""Correctness-gated Linux allocator A/B benchmark for issue #93.

This runner consumes two already-built ``reld`` executables.  Build provenance is supplied by
``ci/allocator_benchmark.sh``; keeping build and replay separate makes the timed environment small
and auditable.  Diagnostic profiler builds are intentionally outside this timing program.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
import platform
import random
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

from ci.benchmark_runner import (
    CONFIGURATIONS,
    DEFAULT_MANIFEST,
    BenchmarkError,
    LinkCommand,
    capture_executable_oracle,
    capture_final_link,
    prune_capture_artifacts,
    release_capture_artifacts,
    replay_command,
)


BASELINE_REVISION = "e2d6be5ae31350862c562d24da01e2147cd5c125"
MIN_SAMPLES = 10
NON_REGRESSION_MARGIN = 0.03
MEASURED_FIELDS = ("wall_seconds", "cpu_seconds", "peak_rss_kib")
PROFILER_ENV_VARS = frozenset(
    {
        "MIMALLOC_PROF",
        "MIMALLOC_PROF_ACTIVE",
        "MIMALLOC_PROF_DUMP_AT_EXIT",
        "MIMALLOC_PROF_PREFIX",
        "MIMALLOC_PROF_SAMPLE_INTERVAL",
        "MIMALLOC_PROF_SAMPLE_RATE",
        "MIMALLOC_PROF_ACCUM",
        "MIMALLOC_PROF_BT_MAX",
        "MIMALLOC_PROF_MAX_BYTES",
        "MIMALLOC_PROF_SEED",
        "MIMALLOC_PROF_DUMP_FORMAT",
        "MIMALLOC_DHAT",
        "MIMALLOC_DHAT_DUMP_AT_EXIT",
        "MIMALLOC_DHAT_MAX_BYTES",
        "RELD_DHAT_OUTPUT",
    }
)


class BenchmarkFailure(RuntimeError):
    pass


def clean_benchmark_environment(environment: dict[str, str]) -> tuple[dict[str, str], list[str]]:
    contaminated = sorted(name for name in PROFILER_ENV_VARS if name in environment)
    return {name: value for name, value in environment.items() if name not in PROFILER_ENV_VARS}, contaminated


def require_clean_parent_environment(environment: dict[str, str]) -> dict[str, str]:
    cleaned, contaminated = clean_benchmark_environment(environment)
    if contaminated:
        raise BenchmarkFailure("profiler environment contamination: " + ", ".join(contaminated))
    # The child receives explicit off values as a second line of defence and as reportable proof.
    cleaned.update({"MIMALLOC_PROF": "0", "MIMALLOC_PROF_ACTIVE": "0", "MIMALLOC_DHAT": "0"})
    return cleaned


def rotated_order(labels: tuple[str, ...], round_index: int, *, seed: int) -> tuple[str, ...]:
    if not labels:
        return ()
    shuffled = list(labels)
    random.Random(seed).shuffle(shuffled)
    offset = round_index % len(shuffled)
    return tuple(shuffled[offset:] + shuffled[:offset])


def bootstrap_improvement_ci(baseline: list[float], candidate: list[float], *, iterations: int = 20_000, seed: int = 93) -> tuple[float, float]:
    if not baseline or len(baseline) != len(candidate):
        raise BenchmarkFailure("paired bootstrap requires equal non-empty samples")
    if any(value <= 0 for value in baseline + candidate):
        raise BenchmarkFailure("paired bootstrap requires strictly positive measurements")
    rng = random.Random(seed)
    improvements: list[float] = []
    for _ in range(iterations):
        indices = rng.choices(range(len(baseline)), k=len(baseline))
        base = statistics.median(baseline[index] for index in indices)
        cand = statistics.median(candidate[index] for index in indices)
        improvements.append(1.0 - cand / base)
    improvements.sort()
    return improvements[int(iterations * 0.025)], improvements[min(iterations - 1, int(iterations * 0.975))]


def bootstrap_geometric_improvement_ci(cells: list[tuple[list[float], list[float]]], *, iterations: int = 20_000, seed: int = 93) -> tuple[float, float]:
    rng = random.Random(seed)
    improvements: list[float] = []
    for _ in range(iterations):
        ratios: list[float] = []
        for baseline, candidate in cells:
            if not baseline or len(baseline) != len(candidate):
                raise BenchmarkFailure("aggregate paired bootstrap requires equal non-empty cells")
            if any(value <= 0 for value in baseline + candidate):
                raise BenchmarkFailure("aggregate paired bootstrap requires strictly positive measurements")
            indices = rng.choices(range(len(baseline)), k=len(baseline))
            base = statistics.median(baseline[index] for index in indices)
            cand = statistics.median(candidate[index] for index in indices)
            ratios.append(cand / base)
        improvements.append(1.0 - math.prod(ratios) ** (1.0 / len(ratios)))
    improvements.sort()
    return improvements[int(iterations * 0.025)], improvements[min(iterations - 1, int(iterations * 0.975))]


def should_keep_allocator(
    configuration_metrics: list[dict[str, dict[str, object]]],
    aggregate_metrics: dict[str, dict[str, object]],
) -> bool:
    every_cell_non_regressing = all(metric["non_regressing"] for configuration in configuration_metrics for metric in configuration.values())
    every_aggregate_non_regressing = all(metric["non_regressing"] for metric in aggregate_metrics.values())
    wall_better = aggregate_metrics["wall_seconds"]["bootstrap_95_ci"][0] > 0
    return wall_better and every_cell_non_regressing and every_aggregate_non_regressing


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _assert_hook_free(candidate: Path, feature_proof: Path) -> dict[str, object]:
    feature_text = feature_proof.read_text(encoding="utf-8")
    forbidden_features = ('mimalloc-pprof feature "pprof"', "mimalloc-pprof/pprof", "mimalloc-pprof-dhat")
    if any(feature in feature_text for feature in forbidden_features):
        raise BenchmarkFailure("candidate Cargo feature proof contains a profiling feature")
    completed = subprocess.run(["nm", "-a", str(candidate)], capture_output=True, text=True, encoding="utf-8", errors="replace", check=False)
    if completed.returncode:
        raise BenchmarkFailure(f"nm could not inspect candidate: {completed.stderr}")
    symbols = [line for line in completed.stdout.splitlines() if "mi_pprof" in line.lower()]
    if symbols:
        raise BenchmarkFailure("candidate contains sampled-pprof symbols; authoritative timing refused")
    return {
        "cargo_feature_tree_sha256": _sha256(feature_proof),
        "cargo_feature_tree": str(feature_proof.resolve()),
        "binary_tool": "nm -a",
        "sampled_pprof_symbols": [],
        "compiled_off": True,
    }


def _assert_no_profile_output(profile_dir: Path) -> None:
    outputs = [path for path in profile_dir.rglob("*") if path.is_file()]
    if outputs:
        raise BenchmarkFailure(f"profiler output contaminated authoritative timing: {outputs}")


def _prepare_command(captured: LinkCommand, binary: Path, output: Path, driver_dir: Path) -> list[str]:
    driver_dir.mkdir(parents=True, exist_ok=True)
    shim = driver_dir / "ld"
    shim.unlink(missing_ok=True)
    shim.symlink_to(binary.resolve())
    command, response = replay_command(
        captured,
        linker=binary.name,
        linker_path=binary,
        output=output,
        response_file=output.with_suffix(".rsp"),
        driver_linker_dir=driver_dir,
    )
    if response is not None:
        raise BenchmarkFailure("Linux replay unexpectedly required a response file")
    command.append("-Wl,--engine=reld")
    return command


def _run_native(
    output: Path,
    oracle_stdout: str,
    oracle_stderr: str,
    environment: dict[str, str],
    cwd: Path,
    *,
    timeout_seconds: float,
) -> None:
    try:
        completed = subprocess.run(
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
    except subprocess.TimeoutExpired as error:
        raise BenchmarkFailure(f"native oracle exceeded {timeout_seconds:.0f}s for {output}") from error
    if completed.returncode != 0 or completed.stdout != oracle_stdout or completed.stderr != oracle_stderr:
        raise BenchmarkFailure(f"native oracle mismatch for {output}: rc={completed.returncode}")


def _timed_link(
    command: list[str],
    *,
    cwd: Path,
    environment: dict[str, str],
    timing_file: Path,
    timeout_seconds: float,
) -> dict[str, float]:
    timing_file.unlink(missing_ok=True)
    wrapped = ["/usr/bin/time", "-f", "%e %U %S %M", "-o", str(timing_file), "--", *command]
    try:
        completed = subprocess.run(
            wrapped,
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        raise BenchmarkFailure(f"link exceeded {timeout_seconds:.0f}s: {command[0]}") from error
    if completed.returncode:
        raise BenchmarkFailure(f"timed link failed:\n{completed.stdout}{completed.stderr}")
    fields = timing_file.read_text(encoding="utf-8").strip().split()
    if len(fields) != 4:
        raise BenchmarkFailure(f"invalid /usr/bin/time record: {fields!r}")
    wall, user, system, rss_kib = map(float, fields)
    return {"wall_seconds": wall, "user_seconds": user, "system_seconds": system, "cpu_seconds": user + system, "peak_rss_kib": rss_kib}


def _summary(samples: list[dict[str, float]]) -> dict[str, object]:
    result: dict[str, object] = {"samples": samples}
    for field in ("wall_seconds", "cpu_seconds", "peak_rss_kib"):
        values = [sample[field] for sample in samples]
        median = statistics.median(values)
        result[f"median_{field}"] = median
        result[f"mad_{field}"] = statistics.median(abs(value - median) for value in values)
    return result


def cleanup_ephemeral_outputs(target_dir: Path, workdir: Path) -> None:
    target_dir = target_dir.resolve()
    for configuration in CONFIGURATIONS:
        release_capture_artifacts(
            profile_dir=target_dir / configuration.profile,
            target_dir=target_dir,
            log=io.StringIO(),
        )
    resolved_workdir = workdir.resolve()
    for pattern in ("identity-*", "timed-*", "time-*.txt", "*.rsp", "app"):
        for path in resolved_workdir.glob(f"**/{pattern}"):
            if path.is_file() and path.resolve().is_relative_to(resolved_workdir):
                path.unlink()
    driver_links = [*resolved_workdir.glob("**/driver/ld"), *resolved_workdir.glob("**/driver-*/ld")]
    for path in driver_links:
        if path.is_symlink() and path.parent.resolve().is_relative_to(resolved_workdir):
            path.unlink()
    for path in sorted(resolved_workdir.glob("**/*"), key=lambda item: len(item.parts), reverse=True):
        if path.is_dir() and (path.name == "driver" or path.name.startswith("driver-")):
            path.rmdir()


def run(args: argparse.Namespace) -> dict[str, object]:
    if sys.platform != "linux":
        raise BenchmarkFailure("allocator timing is authoritative only on Linux")
    if args.trials < MIN_SAMPLES or args.warmup < 0:
        raise BenchmarkFailure(f"at least {MIN_SAMPLES} trials and non-negative warmups are required")
    environment = require_clean_parent_environment(dict(os.environ))
    proof = _assert_hook_free(args.candidate, args.feature_proof)
    workdir = args.workdir.resolve()
    target_dir = args.target_dir.resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    profile_dir = workdir / "profiler-output-must-stay-empty"
    profile_dir.mkdir(parents=True, exist_ok=True)
    _assert_no_profile_output(profile_dir)
    environment["RELD_DHAT_OUTPUT"] = str(profile_dir / "dhat-heap.json")
    report: dict[str, object] = {
        "schema": 1,
        "authoritative": True,
        "baseline_revision": BASELINE_REVISION,
        "candidate_revision": args.source_revision,
        "candidate_tree": args.source_tree,
        "source_archive": {"path": str(args.source_archive.resolve()), "sha256": _sha256(args.source_archive)},
        "baseline_construction": "exact pinned pre-allocator Git archive e2d6be5a; linker implementation matches candidate and allocator-facing manifest/main changes select the system allocator",
        "baseline_archive": {"path": str(args.baseline_archive.resolve()), "sha256": _sha256(args.baseline_archive)},
        "build_environment": {
            "inherited_environment": False,
            "toolchain": "1.95.0",
            "cc": "/usr/bin/clang",
            "cxx": "/usr/bin/clang++",
            "features": ["fork", "plugins", "zstd"],
            "profile": "release",
            "source_date_epoch": "0",
        },
        "profilers": {
            "sampled_pprof": proof,
            "runtime_environment": {name: environment[name] for name in ("MIMALLOC_PROF", "MIMALLOC_PROF_ACTIVE", "MIMALLOC_DHAT")},
            "profile_output_directory": str(profile_dir),
            "profile_output_checked_empty": True,
        },
        "machine": {"platform": platform.platform(), "processor": platform.processor(), "cpu_count": os.cpu_count()},
        "toolchain": {},
        "binaries": {"system": {"path": str(args.baseline.resolve()), "sha256": _sha256(args.baseline)}, "mimalloc": {"path": str(args.candidate.resolve()), "sha256": _sha256(args.candidate)}},
        "warmups": args.warmup,
        "trials": args.trials,
        "seed": args.seed,
        "workload": {
            "manifest": str(args.manifest.resolve()),
            "manifest_sha256": _sha256(args.manifest.resolve()),
            "lockfile": str((args.manifest.resolve().parent / "Cargo.lock")),
            "lockfile_sha256": _sha256(args.manifest.resolve().parent / "Cargo.lock"),
        },
        "configurations": {},
    }
    for tool in ("rustc", "cargo", "clang"):
        completed = subprocess.run([tool, "--version"], env=environment, capture_output=True, text=True, check=True)
        report["toolchain"][tool] = {"path": shutil.which(tool), "version": completed.stdout.strip()}

    labels = ("system", "mimalloc")
    binaries = {"system": args.baseline, "mimalloc": args.candidate}
    log_path = workdir / "capture.log"
    with log_path.open("w", encoding="utf-8") as log:
        for config_index, configuration in enumerate(CONFIGURATIONS):
            print(
                f"allocator benchmark: capturing {configuration.label} (timeout {args.capture_timeout:.0f}s)",
                flush=True,
            )
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
                cwd=args.manifest.resolve().parent,
                environment=environment,
                timeout_seconds=args.native_timeout,
            )
            prune_capture_artifacts(captured, profile_dir=target_dir / configuration.profile, log=log)
            config_dir = workdir / configuration.profile
            config_dir.mkdir(parents=True, exist_ok=True)
            hashes: dict[str, list[str]] = {label: [] for label in labels}
            for label in labels:
                for repetition in range(2):
                    output = config_dir / f"identity-{label}-{repetition}"
                    command = _prepare_command(captured, binaries[label], output, config_dir / f"driver-{label}")
                    try:
                        completed = subprocess.run(
                            command,
                            cwd=args.manifest.parent,
                            env=environment,
                            capture_output=True,
                            text=True,
                            check=False,
                            timeout=args.link_timeout,
                        )
                    except subprocess.TimeoutExpired as error:
                        raise BenchmarkFailure(f"identity link exceeded {args.link_timeout:.0f}s for {configuration.label}/{label}") from error
                    if completed.returncode:
                        raise BenchmarkFailure(f"identity link failed for {configuration.label}/{label}: {completed.stderr}")
                    _run_native(
                        output,
                        oracle.stdout,
                        oracle.stderr,
                        environment,
                        args.manifest.parent,
                        timeout_seconds=args.native_timeout,
                    )
                    hashes[label].append(_sha256(output))
            if len(set(hashes["system"] + hashes["mimalloc"])) != 1:
                raise BenchmarkFailure(f"raw artifact identity failed for {configuration.label}: {hashes}")
            expected_hash = hashes["system"][0]
            for label in labels:
                for repetition in range(2):
                    (config_dir / f"identity-{label}-{repetition}").unlink()

            samples: dict[str, list[dict[str, float]]] = {label: [] for label in labels}
            orders: list[list[str]] = []
            for round_index in range(args.warmup + args.trials):
                order = rotated_order(labels, round_index, seed=args.seed + config_index)
                if round_index >= args.warmup:
                    orders.append(list(order))
                for label in order:
                    output = config_dir / f"timed-{round_index}-{label}"
                    command = _prepare_command(captured, binaries[label], output, config_dir / f"driver-{label}")
                    timing = _timed_link(
                        command,
                        cwd=args.manifest.parent,
                        environment=environment,
                        timing_file=config_dir / f"time-{round_index}-{label}.txt",
                        timeout_seconds=args.link_timeout,
                    )
                    _run_native(
                        output,
                        oracle.stdout,
                        oracle.stderr,
                        environment,
                        args.manifest.parent,
                        timeout_seconds=args.native_timeout,
                    )
                    actual_hash = _sha256(output)
                    if actual_hash != expected_hash:
                        raise BenchmarkFailure(f"timed artifact identity failed for {configuration.label}/{label}/round-{round_index}: {actual_hash} != {expected_hash}")
                    output.unlink()
                    (config_dir / f"time-{round_index}-{label}.txt").unlink()
                    _assert_no_profile_output(profile_dir)
                    if round_index >= args.warmup:
                        samples[label].append(timing)
            metric_improvements: dict[str, dict[str, object]] = {}
            for field in MEASURED_FIELDS:
                baseline_values = [sample[field] for sample in samples["system"]]
                candidate_values = [sample[field] for sample in samples["mimalloc"]]
                ci_low, ci_high = bootstrap_improvement_ci(
                    baseline_values,
                    candidate_values,
                    seed=args.seed + config_index,
                )
                metric_improvements[field] = {
                    "improvement_fraction": 1.0 - statistics.median(candidate_values) / statistics.median(baseline_values),
                    "bootstrap_95_ci": [ci_low, ci_high],
                    "non_regressing": ci_low >= -NON_REGRESSION_MARGIN,
                }
            report["configurations"][configuration.label] = {
                "captured_command": {
                    "executable": captured.executable,
                    "arguments": list(captured.arguments),
                    "cwd": str(args.manifest.resolve().parent),
                },
                "artifact_sha256": hashes["system"][0],
                "identity_runs": hashes,
                "native_oracle": {"stdout": oracle.stdout, "stderr": oracle.stderr, "exit_code": 0},
                "orders": orders,
                "modes": {label: _summary(samples[label]) for label in labels},
                "improvement_fraction": metric_improvements["wall_seconds"]["improvement_fraction"],
                "bootstrap_95_ci": metric_improvements["wall_seconds"]["bootstrap_95_ci"],
                "metric_improvements": metric_improvements,
                "bootstrap": {
                    "method": "paired round bootstrap of median ratio",
                    "iterations": 20_000,
                    "confidence": 0.95,
                },
            }
            result = report["configurations"][configuration.label]
            print(
                f"allocator benchmark: {configuration.label} complete; "
                f"system={result['modes']['system']['median_wall_seconds']:.4f}s "
                f"mimalloc={result['modes']['mimalloc']['median_wall_seconds']:.4f}s "
                f"improvement={result['improvement_fraction']:.2%}",
                flush=True,
            )
            release_capture_artifacts(profile_dir=target_dir / configuration.profile, target_dir=target_dir, log=log)

    aggregate_metrics: dict[str, dict[str, object]] = {}
    for field in MEASURED_FIELDS:
        ratios = []
        aggregate_cells: list[tuple[list[float], list[float]]] = []
        for config in report["configurations"].values():
            baseline = [sample[field] for sample in config["modes"]["system"]["samples"]]
            candidate = [sample[field] for sample in config["modes"]["mimalloc"]["samples"]]
            aggregate_cells.append((baseline, candidate))
            ratios.append(statistics.median(candidate) / statistics.median(baseline))
        aggregate_ci = bootstrap_geometric_improvement_ci(aggregate_cells, seed=args.seed)
        aggregate_metrics[field] = {
            "geometric_improvement_fraction": 1.0 - math.prod(ratios) ** (1.0 / len(ratios)),
            "bootstrap_95_ci": list(aggregate_ci),
            "non_regressing": aggregate_ci[0] >= -NON_REGRESSION_MARGIN,
        }
    keep_allocator = should_keep_allocator(
        [config["metric_improvements"] for config in report["configurations"].values()],
        aggregate_metrics,
    )
    report["aggregate"] = {
        "geometric_improvement_fraction": aggregate_metrics["wall_seconds"]["geometric_improvement_fraction"],
        "bootstrap_95_ci": aggregate_metrics["wall_seconds"]["bootstrap_95_ci"],
        "metrics": aggregate_metrics,
        "bootstrap_method": "paired-within-workload bootstrap of geometric median ratio",
        "bootstrap_iterations": 20_000,
        "non_regression_margin_fraction": NON_REGRESSION_MARGIN,
        "decision": "better" if keep_allocator else "inconclusive_or_worse",
        "keep_allocator": keep_allocator,
    }
    _assert_no_profile_output(profile_dir)
    report["profilers"]["profile_output_verified_empty_after_timing"] = True
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--feature-proof", required=True, type=Path)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--source-tree", required=True)
    parser.add_argument("--source-archive", required=True, type=Path)
    parser.add_argument("--baseline-archive", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--workdir", type=Path, default=Path("target/allocator-benchmark/replay"))
    parser.add_argument("--target-dir", type=Path, default=Path("target/allocator-benchmark/workload"))
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--trials", type=int, default=10)
    parser.add_argument("--seed", type=int, default=93)
    parser.add_argument("--capture-timeout", type=float, default=1_800.0)
    parser.add_argument("--link-timeout", type=float, default=300.0)
    parser.add_argument("--native-timeout", type=float, default=30.0)
    args = parser.parse_args(argv)
    result: dict[str, object] | None = None
    failure: Exception | None = None
    try:
        result = run(args)
    except (BenchmarkFailure, BenchmarkError, OSError, subprocess.SubprocessError) as error:
        failure = error
    finally:
        cleanup_ephemeral_outputs(args.target_dir, args.workdir)
    if failure is not None:
        sys.stderr.write(f"allocator benchmark failed: {failure}\n")
        return 1
    assert result is not None
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
