"""Correctness-gated Linux ELF linker competition evidence.

This module deliberately has no third-party dependencies.  It consumes the checked Clang
corpus lock, provisions checked external-linker archives, establishes artifact identities, and
records interleaved direct-link wall/RSS samples in per-trial cgroup-v2 subtrees.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import shutil
import statistics
import subprocess
import sys
import tarfile
import threading
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

from ci.benchmark_assets import sha256_file
from ci.clang_link_replay import (
    CORPUS_TOKEN,
    OUTPUT_TOKEN,
    ReplayError,
    acquire_archive,
    extract_and_verify,
    link_recipe,
    load_lock,
    native_oracle,
)


LOCK_SCHEMA_VERSION = 1
EXTERNAL_CONTENDERS = ("bfd", "lld", "mold", "wild")
CONTENDER_ORDER = (*EXTERNAL_CONTENDERS, "baseline", "candidate")
MIN_SAMPLES = 10
MIN_WARMUPS = 2
RSS_BACKEND = "cgroup-v2-proc-vmrss-sum"
WALL_CLOCK_BACKEND = "time.perf_counter"
BOOTSTRAP_ITERATIONS = 20_000


class CompetitionError(RuntimeError):
    """A lock, correctness gate, cgroup collector, or statistics contract failed."""


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _require_sha256(value: object, field: str) -> str:
    if not isinstance(value, str) or len(value) != 64 or any(c not in "0123456789abcdef" for c in value):
        raise CompetitionError(f"{field} must be a lowercase SHA-256 digest")
    if value == "0" * 64:
        raise CompetitionError(f"{field} must not be an unpublished placeholder digest")
    return value


def _require_string(value: object, field: str, *, empty: bool = False) -> str:
    if not isinstance(value, str) or (not empty and not value):
        raise CompetitionError(f"{field} must be {'a string' if empty else 'a non-empty string'}")
    return value


def validate_comparator_lock(lock: object) -> dict[str, Any]:
    if not isinstance(lock, dict) or lock.get("schema_version") != LOCK_SCHEMA_VERSION:
        raise CompetitionError(f"schema_version must be {LOCK_SCHEMA_VERSION}")
    comparators = lock.get("comparators")
    if not isinstance(comparators, dict) or tuple(comparators) != EXTERNAL_CONTENDERS:
        raise CompetitionError("comparators must contain exactly bfd, lld, mold, wild in fixed order")
    for label, entry in comparators.items():
        if not isinstance(entry, dict):
            raise CompetitionError(f"comparators.{label} must be an object")
        url = _require_string(entry.get("url"), f"comparators.{label}.url")
        if not url.startswith("https://") or "/releases/download/" not in url or "/latest/" in url:
            raise CompetitionError(f"comparators.{label}.url must be an immutable release URL")
        _require_sha256(entry.get("archive_sha256"), f"comparators.{label}.archive_sha256")
        binary_path = _require_string(entry.get("binary_path"), f"comparators.{label}.binary_path")
        if binary_path.startswith("/") or ".." in Path(binary_path).parts:
            raise CompetitionError(f"comparators.{label}.binary_path must be relative and safe")
        _require_sha256(entry.get("binary_sha256"), f"comparators.{label}.binary_sha256")
        argv = entry.get("version_argv")
        if not isinstance(argv, list) or not argv or not all(isinstance(value, str) for value in argv):
            raise CompetitionError(f"comparators.{label}.version_argv must be a non-empty string array")
        _require_string(entry.get("version_stdout"), f"comparators.{label}.version_stdout", empty=True)
        _require_string(entry.get("version_stderr"), f"comparators.{label}.version_stderr", empty=True)
        recipe = entry.get("recipe")
        if not isinstance(recipe, dict) or recipe.get("remove_arguments") != ["--no-fork"] or recipe.get("extra_arguments") != []:
            raise CompetitionError(f"comparators.{label}.recipe must be the exact default-fork recipe")
    return lock


def load_comparator_lock(path: Path) -> dict[str, Any]:
    try:
        return validate_comparator_lock(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, json.JSONDecodeError) as error:
        raise CompetitionError(f"unable to read comparator lock {path}: {error}") from error


def _download(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": "reld-linux-linker-competition/1"})
    with urllib.request.urlopen(request, timeout=120) as response:  # nosec B310: checked lock URL
        return response.read()


def _safe_extract(archive_path: Path, destination: Path) -> None:
    try:
        with tarfile.open(archive_path, "r:*") as archive:
            root = destination.resolve()
            for member in archive.getmembers():
                target = (root / member.name).resolve()
                if root != target and root not in target.parents:
                    raise CompetitionError(f"unsafe comparator archive member: {member.name}")
            archive.extractall(destination, filter="data")  # noqa: S202: validated paths + data filter
    except (tarfile.TarError, OSError) as error:
        raise CompetitionError(f"unable to extract comparator archive {archive_path}: {error}") from error


def provision_comparators(
    lock: dict[str, Any], output_dir: Path, *, fetch: Callable[[str], bytes] = _download
) -> dict[str, Path]:
    """Fetch each pinned archive, verify every byte, and expose a fixed label path."""
    lock = validate_comparator_lock(lock)
    output_dir.mkdir(parents=True, exist_ok=True)
    result: dict[str, Path] = {}
    for label in EXTERNAL_CONTENDERS:
        entry = lock["comparators"][label]
        data = fetch(entry["url"])
        actual = _sha256_bytes(data)
        if actual != entry["archive_sha256"]:
            raise CompetitionError(f"{label} archive SHA-256 mismatch: expected {entry['archive_sha256']}, got {actual}")
        archive = output_dir / f".{label}.archive"
        staging = output_dir / f".{label}.extract"
        archive.write_bytes(data)
        try:
            if staging.exists():
                shutil.rmtree(staging)
            staging.mkdir()
            _safe_extract(archive, staging)
            source = (staging / entry["binary_path"]).resolve()
            if not source.is_file() or not source.is_relative_to(staging.resolve()):
                raise CompetitionError(f"{label} archive lacks checked binary {entry['binary_path']}")
            if sha256_file(source) != entry["binary_sha256"]:
                raise CompetitionError(f"{label} binary SHA-256 mismatch")
            destination = output_dir / label
            shutil.copyfile(source, destination)
            destination.chmod(destination.stat().st_mode | 0o111)
            completed = subprocess.run([destination, *entry["version_argv"]], capture_output=True, check=False)
            if completed.returncode or completed.stdout.decode("utf-8", "replace") != entry["version_stdout"] or completed.stderr.decode("utf-8", "replace") != entry["version_stderr"]:
                raise CompetitionError(f"{label} exact version output mismatch")
            result[label] = destination
        finally:
            archive.unlink(missing_ok=True)
            if staging.exists():
                shutil.rmtree(staging)
    return result


def first_differing_offset(left: Path, right: Path) -> int | None:
    offset = 0
    with left.open("rb") as left_file, right.open("rb") as right_file:
        while True:
            a, b = left_file.read(1 << 20), right_file.read(1 << 20)
            for index, (one, two) in enumerate(zip(a, b)):
                if one != two:
                    return offset + index
            if len(a) != len(b):
                return offset + min(len(a), len(b))
            if not a:
                return None
            offset += len(a)


def identity_gate(
    contenders: dict[str, Path], *, link: Callable[[str, Path], None], native_oracle: Callable[[Path], bool], artifact_dir: Path
) -> dict[str, Any]:
    """Prove baseline/candidate equivalence and external self-determinism before timing."""
    if tuple(contenders) != CONTENDER_ORDER:
        raise CompetitionError("identity contenders must use fixed bfd,lld,mold,wild,baseline,candidate order")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    artifacts: dict[str, list[Path]] = {}
    for label in ("baseline", "candidate", *EXTERNAL_CONTENDERS):
        paths: list[Path] = []
        for run in (1, 2):
            output = artifact_dir / f"{label}-{run}"
            link(label, output)
            if not output.is_file() or output.stat().st_size == 0 or not native_oracle(output):
                raise CompetitionError(f"{label} identity run {run} failed exact native oracle")
            paths.append(output)
        artifacts[label] = paths
    hashes = {label: [sha256_file(path) for path in paths] for label, paths in artifacts.items()}
    reference = artifacts["baseline"][0]
    for label in ("baseline", "candidate"):
        for path in artifacts[label]:
            difference = first_differing_offset(reference, path)
            if difference is not None:
                failure = {"first_differing_offset": difference, "sha256": hashes, "reference": str(reference), "contender": str(path)}
                (artifact_dir / "identity-failure.json").write_text(json.dumps(failure, indent=2) + "\n", encoding="utf-8")
                raise CompetitionError(f"reld raw artifact mismatch at first differing offset {difference}")
    external: dict[str, Any] = {}
    for label in EXTERNAL_CONTENDERS:
        if hashes[label][0] != hashes[label][1]:
            raise CompetitionError(f"{label} failed two-run self-determinism")
        external[label] = {"sha256": hashes[label][0], "artifacts": [str(path) for path in artifacts[label]]}
    return {"reld_identity": {"sha256": hashes["baseline"][0], "artifacts": [str(path) for label in ("baseline", "candidate") for path in artifacts[label]]}, "comparators": external}


def _rotated(values: tuple[str, ...], offset: int) -> list[str]:
    return list(values[offset % len(values) :] + values[: offset % len(values)])


def round_plan(*, samples: int, warmups: int, seed: int) -> dict[str, Any]:
    if warmups < MIN_WARMUPS:
        raise CompetitionError(f"at least two warmups are required (got {warmups})")
    if samples < MIN_SAMPLES:
        raise CompetitionError(f"at least ten measured rounds are required (got {samples})")
    shuffled = list(CONTENDER_ORDER)
    random.Random(seed).shuffle(shuffled)
    values = tuple(shuffled)
    return {"seed": seed, "warmups": [{"round": index, "order": _rotated(values, index)} for index in range(warmups)], "rounds": [{"round": index, "order": _rotated(values, warmups + index)} for index in range(samples)]}


def _bootstrap_median_ci(values: list[float], *, seed: int) -> list[float]:
    if not values:
        raise CompetitionError("bootstrap requires non-empty samples")
    rng = random.Random(seed)
    medians = []
    for _ in range(BOOTSTRAP_ITERATIONS):
        medians.append(statistics.median(rng.choices(values, k=len(values))))
    medians.sort()
    return [medians[int(BOOTSTRAP_ITERATIONS * 0.025)], medians[min(BOOTSTRAP_ITERATIONS - 1, int(BOOTSTRAP_ITERATIONS * 0.975))]]


def _median_summary(values: list[float], *, seed: int) -> dict[str, Any]:
    if not values:
        raise CompetitionError("cannot summarize empty measurements")
    median = statistics.median(values)
    return {
        "median": median,
        "median_absolute_deviation": statistics.median(abs(value - median) for value in values),
        "min": min(values),
        "max": max(values),
        "bootstrap_95_ci": _bootstrap_median_ci(values, seed=seed),
    }


def _stable_seed(*parts: str) -> int:
    return int.from_bytes(hashlib.sha256("\0".join(parts).encode("utf-8")).digest()[:8], "big")


def _bootstrap_improvement(baseline: list[float], candidate: list[float], *, seed: int) -> list[float]:
    if len(baseline) != len(candidate) or not baseline or any(value <= 0 for value in baseline + candidate):
        raise CompetitionError("paired bootstrap requires equal strictly positive samples")
    rng = random.Random(seed)
    values = []
    for _ in range(BOOTSTRAP_ITERATIONS):
        picks = rng.choices(range(len(baseline)), k=len(baseline))
        values.append(1 - statistics.median(candidate[index] for index in picks) / statistics.median(baseline[index] for index in picks))
    values.sort()
    return [values[int(BOOTSTRAP_ITERATIONS * 0.025)], values[min(BOOTSTRAP_ITERATIONS - 1, int(BOOTSTRAP_ITERATIONS * 0.975))]]


def paired_comparison(baseline: list[dict[str, Any]], candidate: list[dict[str, Any]], *, seed: int) -> dict[str, Any]:
    base = {sample.get("round"): sample for sample in baseline}
    cand = {sample.get("round"): sample for sample in candidate}
    if set(base) != set(cand) or len(base) != len(baseline) or len(cand) != len(candidate):
        raise CompetitionError("paired comparisons require the same round ids exactly once")
    result: dict[str, Any] = {}
    for metric in ("wall_seconds", "peak_rss_kib"):
        baseline_values = [float(base[round_id][metric]) for round_id in sorted(base)]
        candidate_values = [float(cand[round_id][metric]) for round_id in sorted(base)]
        result[metric] = {"improvement_fraction": 1 - statistics.median(candidate_values) / statistics.median(baseline_values), "bootstrap_95_ci": _bootstrap_improvement(baseline_values, candidate_values, seed=seed)}
    return result


def validate_sample(sample: object) -> dict[str, Any]:
    if not isinstance(sample, dict):
        raise CompetitionError("sample must be an object")
    if sample.get("contender") not in CONTENDER_ORDER:
        raise CompetitionError("sample contender is invalid")
    if not isinstance(sample.get("round"), int) or not isinstance(sample.get("position"), int):
        raise CompetitionError("sample round and position are required integers")
    if tuple(sample.get("order", ())) != CONTENDER_ORDER and set(sample.get("order", ())) != set(CONTENDER_ORDER):
        raise CompetitionError("sample order must contain every fixed contender")
    for field in ("wall_seconds", "peak_rss_kib", "cgroup_memory_peak_bytes", "cgroup_cpu_usec"):
        if not isinstance(sample.get(field), (int, float)) or sample[field] <= 0:
            raise CompetitionError(f"sample {field} must be positive")
    backend = sample.get("metric_backend")
    if not isinstance(backend, dict) or backend.get("wall_seconds") != WALL_CLOCK_BACKEND or backend.get("peak_rss_kib") != RSS_BACKEND:
        raise CompetitionError("sample must use validated whole-tree RSS backend")
    _require_sha256(sample.get("output_sha256"), "sample output_sha256")
    return sample


def _read_cpu_usec(path: Path) -> int:
    for line in (path / "cpu.stat").read_text(encoding="utf-8").splitlines():
        key, value = line.split()
        if key == "usage_usec":
            return int(value)
    raise CompetitionError("cgroup cpu.stat has no usage_usec")


def _read_vmrss_kib(pid: str) -> int:
    try:
        for line in Path("/proc").joinpath(pid, "status").read_text(encoding="utf-8").splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except (FileNotFoundError, ProcessLookupError):
        return 0
    return 0


@dataclass
class TrialCgroup:
    path: Path

    @classmethod
    def create(cls, root: Path, label: str) -> "TrialCgroup":
        if not root.is_dir() or not (root / "cgroup.controllers").is_file():
            raise CompetitionError("cgroup root is not a delegated cgroup-v2 directory")
        controllers = (root / "cgroup.controllers").read_text(encoding="utf-8").split()
        enabled = (root / "cgroup.subtree_control").read_text(encoding="utf-8").split()
        if "memory" not in controllers or "cpu" not in controllers or "memory" not in enabled or "cpu" not in enabled:
            raise CompetitionError("cgroup root must delegate enabled memory and cpu controllers")
        path = root / f"reld-link-{label}-{os.getpid()}-{time.time_ns()}"
        path.mkdir()
        for field in ("cgroup.procs", "memory.peak", "cpu.stat"):
            if not (path / field).exists():
                path.rmdir()
                raise CompetitionError(f"trial cgroup lacks {field}")
        (path / "memory.peak").write_text("0\n", encoding="utf-8")
        return cls(path)

    def close(self) -> None:
        if (self.path / "cgroup.procs").read_text(encoding="utf-8").strip():
            raise CompetitionError(f"trial cgroup still contains processes: {self.path}")
        self.path.rmdir()

    def run(self, command: list[str], *, cwd: Path, environment: dict[str, str]) -> tuple[subprocess.CompletedProcess[bytes], float, float, int, int]:
        peak_rss = 0
        seen_pid = False
        stop = threading.Event()

        def sample() -> None:
            nonlocal peak_rss, seen_pid
            while not stop.is_set():
                pids = (self.path / "cgroup.procs").read_text(encoding="utf-8").split()
                if pids:
                    seen_pid = True
                    peak_rss = max(peak_rss, sum(_read_vmrss_kib(pid) for pid in pids))
                time.sleep(0.002)

        cpu_before = _read_cpu_usec(self.path)
        monitor = threading.Thread(target=sample, daemon=True)
        def join_cgroup() -> None:
            (self.path / "cgroup.procs").write_text(f"{os.getpid()}\n", encoding="utf-8")
        started = time.perf_counter()
        try:
            process = subprocess.Popen(command, cwd=cwd, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, preexec_fn=join_cgroup)
        except OSError as error:
            raise CompetitionError(f"unable to launch link command: {error}") from error
        monitor.start()
        stdout, stderr = process.communicate()
        elapsed = time.perf_counter() - started
        stop.set()
        monitor.join()
        # One final sample catches a short process that ended before the monitor's first tick.
        pids = (self.path / "cgroup.procs").read_text(encoding="utf-8").split()
        peak_rss = max(peak_rss, sum(_read_vmrss_kib(pid) for pid in pids))
        if not seen_pid or peak_rss <= 0:
            raise CompetitionError("whole-tree RSS sampler observed no resident trial process")
        completed = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
        return completed, elapsed, float(peak_rss), int((self.path / "memory.peak").read_text(encoding="utf-8")), _read_cpu_usec(self.path) - cpu_before


def _expand(value: str, corpus: Path, output: Path) -> str:
    return value.replace(CORPUS_TOKEN, str(corpus)).replace(OUTPUT_TOKEN, str(output))


def _native_oracle(output: Path, lock: dict[str, Any], corpus: Path, cwd: Path, environment: dict[str, str]) -> bool:
    oracle = native_oracle(lock)
    completed = subprocess.run([_expand(arg, corpus, output) for arg in oracle.arguments], cwd=cwd, env=environment, capture_output=True, check=False)
    return completed.returncode == oracle.exit_code and completed.stdout == oracle.stdout and completed.stderr == oracle.stderr


def _direct_command(lock: dict[str, Any], linker: Path, corpus: Path, output: Path) -> tuple[list[str], Path, dict[str, str]]:
    recipe = link_recipe(lock)
    # The corpus's no-fork switch is a diagnostic-control flag. Competition measures default
    # product mode, so remove it for every contender and record the resulting argv.
    arguments = [_expand(arg, corpus, output) for arg in recipe.arguments if arg != "--no-fork"]
    return [str(linker), *arguments], corpus / recipe.cwd, recipe.environment


def build_report(
    *,
    contenders: dict[str, Path],
    raw_samples: list[dict[str, Any]],
    identity: dict[str, Any],
    plan: dict[str, Any],
    provenance: dict[str, Any],
) -> dict[str, Any]:
    """Build the renderer-facing schema from validated, correctness-gated trial evidence."""
    if tuple(contenders) != CONTENDER_ORDER:
        raise CompetitionError("contenders must use fixed competition order")
    for sample in raw_samples:
        validate_sample(sample)
    samples_by_contender = {
        label: [sample for sample in raw_samples if sample["contender"] == label]
        for label in CONTENDER_ORDER
    }
    if any(not samples for samples in samples_by_contender.values()):
        raise CompetitionError("every contender needs measured raw samples")
    contender_entries: dict[str, dict[str, Any]] = {}
    for label in CONTENDER_ORDER:
        samples = samples_by_contender[label]
        contender_entries[label] = {
            "label": label,
            "path": str(contenders[label]),
            "sha256": sha256_file(contenders[label]),
            "summaries": {
                metric: _median_summary(
                    [float(sample[metric]) for sample in samples],
                    seed=_stable_seed("summary", label, metric),
                )
                for metric in ("wall_seconds", "peak_rss_kib")
            },
        }
    candidate_samples = samples_by_contender["candidate"]
    comparisons = [
        {
            "reference": label,
            "candidate": "candidate",
            "metrics": paired_comparison(
                samples_by_contender[label], candidate_samples, seed=_stable_seed("comparison", label, "candidate")
            ),
        }
        for label in (*EXTERNAL_CONTENDERS, "baseline")
    ]
    return {
        "schema_version": 2,
        "contender_order": list(CONTENDER_ORDER),
        "contenders": contender_entries,
        "comparisons": comparisons,
        "raw_samples": raw_samples,
        "identity": identity,
        "provenance": provenance,
        "metric_scope": {
            "wall_seconds": "direct linker transaction, default fork mode",
            "peak_rss_kib": "maximum summed VmRSS of PIDs in unique cgroup-v2 trial",
            "cgroup_memory_peak_bytes": "diagnostic; not labelled RSS",
            "cgroup_cpu_usec": "whole-tree cgroup-v2 CPU diagnostic",
        },
        "plan": plan,
    }


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        with temporary.open("w", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def write_evidence(report: dict[str, Any], report_path: Path) -> None:
    """Write renderer report and uploaded evidence sidecars without partial JSON files."""
    raw_samples = report.get("raw_samples")
    provenance = report.get("provenance")
    if not isinstance(raw_samples, list) or not isinstance(provenance, dict):
        raise CompetitionError("report lacks raw_samples or provenance evidence")
    _atomic_write(
        report_path.with_name("raw-samples.jsonl"),
        "".join(json.dumps(sample, sort_keys=True, separators=(",", ":")) + "\n" for sample in raw_samples),
    )
    _atomic_write(
        report_path.with_name("provenance.json"),
        json.dumps(provenance, indent=2, sort_keys=True) + "\n",
    )
    _atomic_write(report_path, json.dumps(report, indent=2, sort_keys=True) + "\n")


def run_replay(args: argparse.Namespace) -> dict[str, Any]:
    if sys.platform != "linux":
        raise CompetitionError("Linux competitive replay is Linux-only")
    if args.samples < MIN_SAMPLES or args.warmups < MIN_WARMUPS:
        round_plan(samples=args.samples, warmups=args.warmups, seed=103)
    corpus_lock = load_lock(args.corpus_lock)
    comparator_lock = load_comparator_lock(args.comparator_lock)
    workdir = args.workdir.resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    archive = acquire_archive(corpus_lock, archive_override=None, destination=workdir / "corpus.tar.zst")
    corpus = workdir / "corpus"
    extract_and_verify(corpus_lock, archive, corpus)
    external = provision_comparators(comparator_lock, args.comparators_dir.resolve())
    contenders = {**external, "baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    if tuple(contenders) != CONTENDER_ORDER or any(not value.is_file() for value in contenders.values()):
        raise CompetitionError("all fixed contender executables must exist")
    recipe = link_recipe(corpus_lock)
    environment = dict(recipe.environment)
    cwd = corpus / recipe.cwd
    def link(label: str, output: Path) -> None:
        command, _, env = _direct_command(corpus_lock, contenders[label], corpus, output)
        completed = subprocess.run(command, cwd=cwd, env=env, capture_output=True, check=False)
        if completed.returncode != 0 or completed.stdout != recipe.stdout or completed.stderr != recipe.stderr:
            raise CompetitionError(f"{label} link failed exact link-output oracle")
    identity = identity_gate(contenders, link=link, native_oracle=lambda output: _native_oracle(output, corpus_lock, corpus, cwd, environment), artifact_dir=workdir / "identity-artifacts")
    plan = round_plan(samples=args.samples, warmups=args.warmups, seed=103)
    expected_hashes = {label: identity["reld_identity"]["sha256"] if label in {"baseline", "candidate"} else identity["comparators"][label]["sha256"] for label in CONTENDER_ORDER}
    samples: list[dict[str, Any]] = []
    for phase, rows in (("warmup", plan["warmups"]), ("sample", plan["rounds"])):
        for row in rows:
            for position, label in enumerate(row["order"]):
                output = workdir / "outputs" / f"{phase}-{row['round']}-{label}"
                output.parent.mkdir(parents=True, exist_ok=True)
                output.unlink(missing_ok=True)
                trial = TrialCgroup.create(args.cgroup_root.resolve(), f"{phase}-{row['round']}-{label}")
                try:
                    command, command_cwd, command_env = _direct_command(corpus_lock, contenders[label], corpus, output)
                    completed, wall, rss, memory_peak, cpu = trial.run(command, cwd=command_cwd, environment=command_env)
                    if completed.returncode or completed.stdout != recipe.stdout or completed.stderr != recipe.stderr:
                        raise CompetitionError(f"{label} timed link failed exact link-output oracle")
                    actual_hash = sha256_file(output)
                    if actual_hash != expected_hashes[label] or not _native_oracle(output, corpus_lock, corpus, cwd, environment):
                        raise CompetitionError(f"{label} timed output failed identity/native oracle")
                    if phase == "sample":
                        sample = {"contender": label, "round": row["round"], "position": position, "order": row["order"], "wall_seconds": wall, "peak_rss_kib": rss, "cgroup_memory_peak_bytes": memory_peak, "cgroup_cpu_usec": cpu, "metric_backend": {"wall_seconds": WALL_CLOCK_BACKEND, "peak_rss_kib": RSS_BACKEND}, "output_sha256": actual_hash}
                        validate_sample(sample)
                        samples.append(sample)
                finally:
                    output.unlink(missing_ok=True)
                    trial.close()
    return build_report(
        contenders=contenders,
        raw_samples=samples,
        identity=identity,
        plan=plan,
        provenance={
            "corpus_lock": {"path": str(args.corpus_lock.resolve()), "sha256": sha256_file(args.corpus_lock)},
            "comparator_lock": {"path": str(args.comparator_lock.resolve()), "sha256": sha256_file(args.comparator_lock)},
            "execution": {
                "removed_recipe_arguments": ["--no-fork"],
                "metric_backends": {"wall_seconds": WALL_CLOCK_BACKEND, "peak_rss_kib": RSS_BACKEND},
                "cgroup_root": str(args.cgroup_root.resolve()),
            },
        },
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ci.linux_linker_competition")
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("validate-lock")
    validate.add_argument("--lock", type=Path, required=True)
    provision = commands.add_parser("provision")
    provision.add_argument("--lock", type=Path, required=True)
    provision.add_argument("--output-dir", type=Path, required=True)
    replay = commands.add_parser("replay")
    replay.add_argument("--corpus-lock", type=Path, required=True)
    replay.add_argument("--comparator-lock", type=Path, required=True)
    replay.add_argument("--baseline", type=Path, required=True)
    replay.add_argument("--candidate", type=Path, required=True)
    replay.add_argument("--comparators-dir", type=Path, required=True)
    replay.add_argument("--cgroup-root", type=Path, required=True)
    replay.add_argument("--workdir", type=Path, required=True)
    replay.add_argument("--report", type=Path, required=True)
    replay.add_argument("--samples", type=int, default=MIN_SAMPLES)
    replay.add_argument("--warmups", type=int, default=MIN_WARMUPS)
    args = parser.parse_args(argv)
    try:
        if args.command == "validate-lock":
            validate_comparator_lock(load_comparator_lock(args.lock))
            return 0
        if args.command == "provision":
            provision_comparators(load_comparator_lock(args.lock), args.output_dir)
            return 0
        report = run_replay(args)
        write_evidence(report, args.report)
        return 0
    except (CompetitionError, ReplayError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"linux linker competition failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
