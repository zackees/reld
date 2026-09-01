"""Correctness-gated Linux ELF linker competition evidence.

This module deliberately has no third-party dependencies.  It consumes the checked Clang
corpus lock, provisions checked external-linker archives, establishes artifact identities, and
records interleaved direct-link wall/RSS samples in per-trial cgroup-v2 subtrees.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import select
import shutil
import statistics
import subprocess
import sys
import tarfile
import threading
import time
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

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
CONTENDER_LABELS = {
    "bfd": "GNU bfd",
    "lld": "LLD",
    "mold": "mold",
    "wild": "Wild",
    "baseline": "reld baseline",
    "candidate": "reld candidate",
}
RUNNER_ENV_ALLOWLIST = (
    "BASELINE_SHA",
    "CI",
    "GITHUB_ACTIONS",
    "GITHUB_RUN_ATTEMPT",
    "GITHUB_RUN_ID",
    "GITHUB_SHA",
    "ImageOS",
    "ImageVersion",
    "RUNNER_ARCH",
    "RUNNER_ENVIRONMENT",
    "RUNNER_OS",
)
RECIPE_ENV_ALLOWLIST = frozenset({"LANG", "LC_ALL", "LC_MESSAGES", "LC_NUMERIC", "PATH", "SOURCE_DATE_EPOCH", "TZ"})
MIN_SAMPLES = 10
MIN_WARMUPS = 2
RSS_BACKEND = "cgroup-v2-proc-vmrss-sum"
WALL_CLOCK_BACKEND = "time.perf_counter"
BOOTSTRAP_ITERATIONS = 20_000
LAUNCHER_READY_TIMEOUT_SECONDS = 30
RSS_SELF_TEST_ALLOCATION_BYTES = 16 * 1024 * 1024
RSS_SELF_TEST_SLEEP_SECONDS = 1.0
RSS_SELF_TEST_LOWER_KIB = RSS_SELF_TEST_ALLOCATION_BYTES * 2 * 3 // (4 * 1024)
RSS_SELF_TEST_UPPER_KIB = RSS_SELF_TEST_ALLOCATION_BYTES * 2 // 1024 + 96 * 1024
_CGROUP_LAUNCHER = (
    "import os\n"
    "import sys\n"
    "with open(sys.argv[1], 'w', encoding='ascii') as cgroup_procs:\n"
    "    cgroup_procs.write(f'{os.getpid()}\\n')\n"
    "os.write(int(sys.argv[2]), b'R')\n"
    "os.close(int(sys.argv[2]))\n"
    "if os.read(int(sys.argv[3]), 1) != b'G':\n"
    "    raise SystemExit(125)\n"
    "os.close(int(sys.argv[3]))\n"
    "os.execvpe(sys.argv[4], sys.argv[4:], os.environ)\n"
)
_RSS_SELF_TEST_CHILD = (
    "import sys\n"
    "allocation = bytearray(int(sys.argv[1]))\n"
    "for index in range(0, len(allocation), 4096):\n"
    "    allocation[index] = 1\n"
    "import time\n"
    "time.sleep(float(sys.argv[2]))\n"
)
_RSS_SELF_TEST_PARENT = (
    "from pathlib import Path\n"
    "import os\n"
    "import subprocess\n"
    "import sys\n"
    "import time\n"
    "allocation = bytearray(int(sys.argv[1]))\n"
    "for index in range(0, len(allocation), 4096):\n"
    "    allocation[index] = 1\n"
    "child = subprocess.Popen([sys.executable, '-c', sys.argv[3], sys.argv[1], sys.argv[2]])\n"
    "Path(sys.argv[4]).write_text(f'{os.getpid()} {child.pid}\\n', encoding='ascii')\n"
    "time.sleep(float(sys.argv[2]))\n"
    "raise SystemExit(child.wait())\n"
)


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
            # Member paths above and the tarfile data filter make extraction intentionally safe.
            archive.extractall(destination, filter="data")
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
    contenders: dict[str, Path], *, link: Callable[[str, Path], None], native_oracle: Callable[[Path], bool], artifact_dir: Path, output_path: Path
) -> dict[str, Any]:
    """Prove baseline/candidate equivalence and external self-determinism before timing."""
    if tuple(contenders) != CONTENDER_ORDER:
        raise CompetitionError("identity contenders must use fixed bfd,lld,mold,wild,baseline,candidate order")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    artifacts: dict[str, list[Path]] = {}
    for label in ("baseline", "candidate", *EXTERNAL_CONTENDERS):
        paths: list[Path] = []
        for run in (1, 2):
            output_path.unlink(missing_ok=True)
            link(label, output_path)
            if not output_path.is_file() or output_path.stat().st_size == 0 or not native_oracle(output_path):
                raise CompetitionError(f"{label} identity run {run} failed exact native oracle")
            retained = artifact_dir / f"{label}-{run}"
            shutil.copyfile(output_path, retained)
            paths.append(retained)
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
    order = sample.get("order")
    if not isinstance(order, list) or len(order) != len(CONTENDER_ORDER) or set(order) != set(CONTENDER_ORDER):
        raise CompetitionError("sample order must be an exact fixed-contender permutation")
    if sample["position"] < 0 or sample["position"] >= len(CONTENDER_ORDER) or order[sample["position"]] != sample["contender"]:
        raise CompetitionError("sample position must select its contender from order")
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


def cgroup_launcher_command(cgroup: Path, ready_fd: int, go_fd: int, command: list[str]) -> list[str]:
    """Use a fresh stdlib process to attach itself before execing the measured linker."""
    if not command:
        raise CompetitionError("cannot launch an empty linker command")
    return [sys.executable, "-c", _CGROUP_LAUNCHER, str(cgroup / "cgroup.procs"), str(ready_fd), str(go_fd), *command]


def _close_fd(descriptor: int | None) -> None:
    if descriptor is not None:
        try:
            os.close(descriptor)
        except OSError:
            pass


@dataclass
class CgroupLauncher:
    """A cgroup-attached process held before exec until measurement begins."""

    process: subprocess.Popen[bytes]
    go_fd: int | None

    @classmethod
    def start(cls, cgroup: Path, command: list[str], *, cwd: Path, environment: dict[str, str]) -> CgroupLauncher:
        ready_read, ready_write = os.pipe()
        go_read, go_write = os.pipe()
        process: subprocess.Popen[bytes] | None = None
        try:
            process = subprocess.Popen(
                cgroup_launcher_command(cgroup, ready_write, go_read, command),
                cwd=cwd,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                pass_fds=(ready_write, go_read),
            )
        except OSError as error:
            raise CompetitionError(f"unable to launch cgroup handshake process: {error}") from error
        finally:
            _close_fd(ready_write)
            _close_fd(go_read)
        try:
            readable, _, _ = select.select([ready_read], [], [], LAUNCHER_READY_TIMEOUT_SECONDS)
            if not readable:
                raise CompetitionError("cgroup launcher did not become ready before the measurement timeout")
            ready = os.read(ready_read, 1)
            if ready != b"R":
                stdout, stderr = process.communicate()
                raise CompetitionError(
                    "cgroup launcher exited before readiness: "
                    f"returncode={process.returncode}, stdout={stdout!r}, stderr={stderr!r}"
                )
        except BaseException:
            _close_fd(go_write)
            if process.poll() is None:
                process.terminate()
            process.communicate()
            raise
        finally:
            _close_fd(ready_read)
        return cls(process, go_write)

    def release(self) -> None:
        if self.go_fd is None:
            raise CompetitionError("cgroup launcher was released more than once")
        go_fd, self.go_fd = self.go_fd, None
        try:
            os.write(go_fd, b"G")
        except OSError as error:
            raise CompetitionError(f"unable to release ready cgroup launcher: {error}") from error
        finally:
            _close_fd(go_fd)

    def terminate(self) -> None:
        _close_fd(self.go_fd)
        self.go_fd = None
        if self.process.poll() is None:
            self.process.terminate()
        self.process.communicate()


@dataclass
class TrialCgroup:
    path: Path

    @classmethod
    def create(cls, root: Path, label: str) -> TrialCgroup:
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
                pids, summed_rss_kib = _summed_vmrss_kib(self.path)
                if pids:
                    seen_pid = True
                    peak_rss = max(peak_rss, summed_rss_kib)
                time.sleep(0.002)

        cpu_before = _read_cpu_usec(self.path)
        monitor = threading.Thread(target=sample, daemon=True)
        launcher = CgroupLauncher.start(self.path, command, cwd=cwd, environment=environment)
        try:
            monitor.start()
            # The launcher is cgroup-attached and blocked at this point; do not charge Python
            # startup or cgroup setup to the direct linker wall-clock transaction.
            started = time.perf_counter()
            launcher.release()
            stdout, stderr = launcher.process.communicate()
            elapsed = time.perf_counter() - started
        except BaseException:
            launcher.terminate()
            raise
        finally:
            stop.set()
            monitor.join()
        # One final sample catches a short process that ended before the monitor's first tick.
        _pids, summed_rss_kib = _summed_vmrss_kib(self.path)
        peak_rss = max(peak_rss, summed_rss_kib)
        if not seen_pid or peak_rss <= 0:
            raise CompetitionError("whole-tree RSS sampler observed no resident trial process")
        completed = subprocess.CompletedProcess(command, launcher.process.returncode, stdout, stderr)
        return completed, elapsed, float(peak_rss), int((self.path / "memory.peak").read_text(encoding="utf-8")), _read_cpu_usec(self.path) - cpu_before


def _summed_vmrss_kib(cgroup: Path) -> tuple[set[str], int]:
    pids = set((cgroup / "cgroup.procs").read_text(encoding="utf-8").split())
    return pids, sum(_read_vmrss_kib(pid) for pid in pids)


def _validate_rss_probe(*, parent_pid: str, child_pid: str, observed_pids: set[str], peak_rss_kib: int) -> None:
    if not {parent_pid, child_pid}.issubset(observed_pids):
        raise CompetitionError("RSS self-test did not observe both parent and child in the trial cgroup")
    if not RSS_SELF_TEST_LOWER_KIB <= peak_rss_kib <= RSS_SELF_TEST_UPPER_KIB:
        raise CompetitionError(
            "RSS self-test summed VmRSS outside the validated allocation range "
            f"[{RSS_SELF_TEST_LOWER_KIB}, {RSS_SELF_TEST_UPPER_KIB}] KiB: {peak_rss_kib} KiB"
        )


def validate_rss_measurement(cgroup_root: Path, workdir: Path) -> dict[str, int]:
    """Fail closed unless the live cgroup sampler sees both known-allocation processes."""
    pid_file = workdir / f"rss-self-test-{os.getpid()}-{time.time_ns()}.pids"
    trial = TrialCgroup.create(cgroup_root, "rss-self-test")
    launcher: CgroupLauncher | None = None
    try:
        command = [
            sys.executable,
            "-c",
            _RSS_SELF_TEST_PARENT,
            str(RSS_SELF_TEST_ALLOCATION_BYTES),
            str(RSS_SELF_TEST_SLEEP_SECONDS),
            _RSS_SELF_TEST_CHILD,
            str(pid_file),
        ]
        launcher = CgroupLauncher.start(trial.path, command, cwd=workdir, environment=dict(os.environ))
        launcher.release()
        deadline = time.monotonic() + LAUNCHER_READY_TIMEOUT_SECONDS
        while not pid_file.is_file() and launcher.process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.002)
        if not pid_file.is_file():
            stdout, stderr = launcher.process.communicate()
            raise CompetitionError(f"RSS self-test did not publish parent/child PIDs: {stdout!r} {stderr!r}")
        parent_pid, child_pid = pid_file.read_text(encoding="ascii").split()
        observed_pids: set[str] = set()
        peak_rss_kib = 0
        while launcher.process.poll() is None:
            pids, summed_rss_kib = _summed_vmrss_kib(trial.path)
            observed_pids.update(pids)
            peak_rss_kib = max(peak_rss_kib, summed_rss_kib)
            time.sleep(0.002)
        stdout, stderr = launcher.process.communicate()
        if launcher.process.returncode != 0 or stdout or stderr:
            raise CompetitionError(f"RSS self-test command failed: {stdout!r} {stderr!r}")
        _validate_rss_probe(
            parent_pid=parent_pid,
            child_pid=child_pid,
            observed_pids=observed_pids,
            peak_rss_kib=peak_rss_kib,
        )
        return {
            "allocation_bytes_per_process": RSS_SELF_TEST_ALLOCATION_BYTES,
            "peak_rss_kib": peak_rss_kib,
            "lower_bound_kib": RSS_SELF_TEST_LOWER_KIB,
            "upper_bound_kib": RSS_SELF_TEST_UPPER_KIB,
        }
    finally:
        if launcher is not None and launcher.process.poll() is None:
            launcher.terminate()
        pid_file.unlink(missing_ok=True)
        trial.close()


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
    return _direct_command_from_recipe(recipe, linker, corpus, output)


def workload_from_corpus_lock(lock: dict[str, Any]) -> dict[str, Any]:
    """Expose checked corpus provenance directly instead of making a renderer infer it."""
    source = lock["source"]
    archive = lock["archive"]
    tag = source["tag"]
    return {
        "id": f"{tag}-clang-final-link",
        "source_tag": tag,
        "source_repository": source["repository"],
        "source_peeled_commit": source["peeled_commit"],
        "platform": lock["platform"],
        "archive": {"url": archive["url"], "sha256": archive["sha256"], "bytes": archive["bytes"]},
    }


def _cpu_model(cpuinfo: str) -> str | None:
    for line in cpuinfo.splitlines():
        key, separator, value = line.partition(":")
        if separator and key.strip() in {"model name", "Hardware"}:
            return value.strip()
    return None


def _memtotal_kib(meminfo: str) -> int | None:
    for line in meminfo.splitlines():
        key, separator, value = line.partition(":")
        if key == "MemTotal" and separator:
            fields = value.split()
            if len(fields) >= 2 and fields[1] == "kB" and fields[0].isdigit():
                return int(fields[0])
    return None


def _pressure_snapshot(pressure: str) -> dict[str, dict[str, float | int]]:
    snapshot: dict[str, dict[str, float | int]] = {}
    for line in pressure.splitlines():
        fields = line.split()
        if not fields:
            continue
        values: dict[str, float | int] = {}
        for field in fields[1:]:
            key, separator, value = field.partition("=")
            if separator and key in {"avg10", "avg60", "avg300", "total"}:
                values[key] = int(value) if key == "total" else float(value)
        if fields[0] in {"some", "full"} and values:
            snapshot[fields[0]] = values
    return snapshot


def _mount_for_path(mountinfo: str, path: Path) -> dict[str, str] | None:
    resolved = str(path.resolve())
    matches: list[dict[str, str]] = []
    for line in mountinfo.splitlines():
        fields = line.split()
        if "-" not in fields:
            continue
        dash = fields.index("-")
        if len(fields) <= dash + 3 or len(fields) < 6:
            continue
        mount_point = fields[4].replace("\\040", " ")
        if resolved == mount_point or resolved.startswith(mount_point.rstrip("/") + "/"):
            matches.append(
                {
                    "mount_point": mount_point,
                    "filesystem": fields[dash + 1],
                    "source": fields[dash + 2],
                    "mount_options": fields[5],
                    "super_options": fields[dash + 3],
                }
            )
    return max(matches, key=lambda match: len(match["mount_point"])) if matches else None


def _allowlisted_recipe_environment(environment: dict[str, str]) -> dict[str, str]:
    unexpected = set(environment) - RECIPE_ENV_ALLOWLIST
    if unexpected:
        raise CompetitionError(f"recipe environment includes non-allowlisted keys: {sorted(unexpected)}")
    return {key: environment[key] for key in sorted(environment)}


def _optional_read(path: Path, read_text: Callable[[Path], str]) -> str | None:
    try:
        return read_text(path)
    except OSError:
        return None


def capture_host_provenance(
    *,
    corpus: Path,
    workdir: Path,
    cgroup_root: Path,
    output_path: Path,
    recipe: Any,
    contenders: dict[str, Path],
    read_text: Callable[[Path], str] = lambda path: path.read_text(encoding="utf-8"),
) -> dict[str, Any]:
    """Capture bounded, non-secret runtime evidence immediately before measurement work."""
    uname = os.uname()
    try:
        affinity = sorted(os.sched_getaffinity(0))
    except (AttributeError, OSError):
        affinity = []
    mountinfo = read_text(Path("/proc/self/mountinfo"))
    commands = {}
    for label in CONTENDER_ORDER:
        argv, cwd, environment = _direct_command_from_recipe(recipe, contenders[label], corpus, output_path)
        commands[label] = {"argv": argv, "cwd": str(cwd), "environment": _allowlisted_recipe_environment(environment)}
    return {
        "runner_image_env": {key: os.environ[key] for key in RUNNER_ENV_ALLOWLIST if key in os.environ},
        "runtime": {"python_version": sys.version, "python_implementation": sys.implementation.name},
        "host": {
            "uname": {"sysname": uname.sysname, "release": uname.release, "version": uname.version, "machine": uname.machine},
            "cpu_model": _cpu_model(read_text(Path("/proc/cpuinfo"))),
            "cpu_count": os.cpu_count(),
            "sched_affinity": affinity,
            "cpuset_effective": (_optional_read(cgroup_root / "cpuset.cpus.effective", read_text) or "").strip() or None,
            "memtotal_kib": _memtotal_kib(read_text(Path("/proc/meminfo"))),
            "loadavg": read_text(Path("/proc/loadavg")).strip(),
            "pressure": {name: _pressure_snapshot(read_text(Path("/proc/pressure") / name)) for name in ("cpu", "memory", "io")},
        },
        "filesystems": {"corpus": _mount_for_path(mountinfo, corpus), "workdir": _mount_for_path(mountinfo, workdir)},
        "cgroup": {
            "path": str(cgroup_root),
            "controllers": read_text(cgroup_root / "cgroup.controllers").split(),
            "subtree_control": read_text(cgroup_root / "cgroup.subtree_control").split(),
        },
        "commands": commands,
    }


def _direct_command_from_recipe(recipe: Any, linker: Path, corpus: Path, output: Path) -> tuple[list[str], Path, dict[str, str]]:
    arguments = [_expand(arg, corpus, output) for arg in recipe.arguments if arg != "--no-fork"]
    return [str(linker), *arguments], corpus / recipe.cwd, dict(recipe.environment)


def artifact_comparison_policy() -> dict[str, Any]:
    """Declare the only artifact-equivalence reference without conflating performance peers."""
    return {
        "artifact_reference": "baseline",
        "baseline": {
            "artifact_reference": True,
            "role": "exact pre-change reld artifact reference",
        },
        "wild": {
            "role": "external performance comparator only",
            "artifact_reference": False,
            "artifact_equivalence_claim": False,
            "disclaimer": (
                "Wild is an external performance comparator only. Exact pre-change reld baseline "
                "is the artifact reference; no Wild/reld equivalence claim is made because reld "
                "intentionally changed build-ID/output-layout policy in PR #76."
            ),
        },
    }


def build_report(
    *,
    contenders: dict[str, Path],
    raw_samples: list[dict[str, Any]],
    identity: dict[str, Any],
    plan: dict[str, Any],
    provenance: dict[str, Any],
    workload: dict[str, Any],
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
            "label": CONTENDER_LABELS[label],
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
    policy = artifact_comparison_policy()
    return {
        "schema_version": 2,
        "contender_order": list(CONTENDER_ORDER),
        "contenders": contender_entries,
        "comparisons": comparisons,
        "raw_samples": raw_samples,
        "workload": workload,
        "artifact_comparison_policy": policy,
        "identity": identity,
        "provenance": {**provenance, "artifact_comparison_policy": policy},
        "metric_scope": {
            "wall_seconds": "direct linker transaction, default fork mode",
            "peak_rss_kib": "maximum summed VmRSS of PIDs in unique cgroup-v2 trial; validated by parent+child allocation preflight",
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
    rss_self_test = validate_rss_measurement(args.cgroup_root.resolve(), workdir)
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
    operation_output = workdir / "link-output"
    host_provenance = capture_host_provenance(
        corpus=corpus,
        workdir=workdir,
        cgroup_root=args.cgroup_root.resolve(),
        output_path=operation_output,
        recipe=recipe,
        contenders=contenders,
    )
    def link(label: str, output: Path) -> None:
        command, _, env = _direct_command(corpus_lock, contenders[label], corpus, output)
        completed = subprocess.run(command, cwd=cwd, env=env, capture_output=True, check=False)
        if completed.returncode != 0 or completed.stdout != recipe.stdout or completed.stderr != recipe.stderr:
            raise CompetitionError(f"{label} link failed exact link-output oracle")
    identity = identity_gate(contenders, link=link, native_oracle=lambda output: _native_oracle(output, corpus_lock, corpus, cwd, environment), artifact_dir=workdir / "identity-artifacts", output_path=operation_output)
    plan = round_plan(samples=args.samples, warmups=args.warmups, seed=103)
    expected_hashes = {label: identity["reld_identity"]["sha256"] if label in {"baseline", "candidate"} else identity["comparators"][label]["sha256"] for label in CONTENDER_ORDER}
    samples: list[dict[str, Any]] = []
    for phase, rows in (("warmup", plan["warmups"]), ("sample", plan["rounds"])):
        for row in rows:
            for position, label in enumerate(row["order"]):
                output = operation_output
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
                "effective_output_path": str(operation_output),
                "metric_backends": {"wall_seconds": WALL_CLOCK_BACKEND, "peak_rss_kib": RSS_BACKEND},
                "cgroup_root": str(args.cgroup_root.resolve()),
                "rss_self_test": rss_self_test,
            },
            "host": host_provenance,
        },
        workload=workload_from_corpus_lock(corpus_lock),
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ci.linux_linker_competition")
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("validate-lock")
    validate.add_argument("--lock", type=Path, required=True)
    provision = commands.add_parser("provision")
    provision.add_argument("--lock", type=Path, required=True)
    provision.add_argument("--output-dir", type=Path, required=True)
    self_test = commands.add_parser("self-test")
    self_test.add_argument("--cgroup-root", type=Path, required=True)
    self_test.add_argument("--workdir", type=Path, required=True)
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
        if args.command == "self-test":
            args.workdir.mkdir(parents=True, exist_ok=True)
            print(json.dumps(validate_rss_measurement(args.cgroup_root.resolve(), args.workdir.resolve()), sort_keys=True))
            return 0
        report = run_replay(args)
        write_evidence(report, args.report)
        return 0
    except (CompetitionError, ReplayError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"linux linker competition failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
