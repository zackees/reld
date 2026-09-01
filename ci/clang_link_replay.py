"""Validate and replay the immutable Linux LLVM/Clang final-link corpus.

This is intentionally separate from :mod:`ci.benchmark_runner`: the published benchmark matrix
builds a small project on every supported platform, while this runner consumes one large,
precompiled Linux corpus.  Archive acquisition, extraction, native execution, and evidence
writing are outside every measured interval.  Only the final linker process is timed.

The repository does not contain a placeholder URL or digest.  Until a real corpus is published,
pass its lock with ``--lock`` and its local archive with ``--archive``.  A published lock may name
an immutable GitHub Release URL, in which case ``--archive`` is optional.
"""

from __future__ import annotations

import argparse
import json
import math
import platform
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Callable

from ci.benchmark_assets import extract_archive, sha256_file


LOCK_SCHEMA_VERSION = 1
OUTPUT_TOKEN = "{OUTPUT}"
CORPUS_TOKEN = "{CORPUS}"
TARGET_SECONDS = 30.0
IDENTITY_RUNS_PER_SIDE = 2
WARMUP_RUNS = 1
CHUNK = 1 << 20


class ReplayError(RuntimeError):
    """The corpus lock, artifact gate, native oracle, or replay failed."""


@dataclass(frozen=True)
class NativeOracle:
    arguments: tuple[str, ...]
    exit_code: int
    stdout: bytes
    stderr: bytes


@dataclass(frozen=True)
class LinkRecipe:
    arguments: tuple[str, ...]
    cwd: str
    environment: dict[str, str]
    stdout: bytes
    stderr: bytes


def _require_dict(value: object, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReplayError(f"{name} must be an object")
    return value


def _require_list(value: object, name: str) -> list[Any]:
    if not isinstance(value, list):
        raise ReplayError(f"{name} must be an array")
    return value


def _require_string(value: object, name: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        suffix = " a string" if allow_empty else " a non-empty string"
        raise ReplayError(f"{name} must be{suffix}")
    return value


def _require_int(value: object, name: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise ReplayError(f"{name} must be an integer >= {minimum}")
    return value


def _require_sha256(value: object, name: str) -> str:
    digest = _require_string(value, name)
    if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
        raise ReplayError(f"{name} must be a lowercase SHA-256 digest")
    return digest


def _require_git_oid(value: object, name: str) -> str:
    oid = _require_string(value, name)
    if len(oid) != 40 or any(character not in "0123456789abcdef" for character in oid):
        raise ReplayError(f"{name} must be a full lowercase 40-character Git object ID")
    return oid


def _safe_relative_path(value: object, name: str, *, allow_dot: bool = False) -> str:
    raw = _require_string(value, name)
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or "\\" in raw:
        raise ReplayError(f"{name} must be a safe POSIX path relative to the corpus root")
    if (raw == "." and not allow_dot) or (raw != "." and str(path) != raw):
        raise ReplayError(f"{name} must be a normalized POSIX relative path")
    return raw


def _require_string_map(value: object, name: str) -> dict[str, str]:
    mapping = _require_dict(value, name)
    result: dict[str, str] = {}
    for key, item in mapping.items():
        if not isinstance(key, str) or not key:
            raise ReplayError(f"{name} keys must be non-empty strings")
        result[key] = _require_string(item, f"{name}.{key}", allow_empty=True)
    return result


def validate_lock(lock: dict[str, Any]) -> None:
    """Validate the complete immutable-corpus contract without touching the network."""
    if lock.get("schema_version") != LOCK_SCHEMA_VERSION:
        raise ReplayError(f"schema_version must be {LOCK_SCHEMA_VERSION}")
    if lock.get("platform") != "x86_64-unknown-linux-gnu":
        raise ReplayError("platform must be exactly 'x86_64-unknown-linux-gnu'")

    source = _require_dict(lock.get("source"), "source")
    for field in ("repository", "tag"):
        _require_string(source.get(field), f"source.{field}")
    _require_git_oid(source.get("tag_object"), "source.tag_object")
    _require_git_oid(source.get("peeled_commit"), "source.peeled_commit")

    builder = _require_dict(lock.get("builder"), "builder")
    image = _require_string(builder.get("image"), "builder.image")
    image_digest = image.rsplit("@sha256:", 1)[1] if "@sha256:" in image else ""
    if len(image_digest) != 64 or any(
        character not in "0123456789abcdef" for character in image_digest
    ):
        raise ReplayError("builder.image must pin an image by @sha256 digest")
    packages = _require_list(builder.get("packages"), "builder.packages")
    if not packages:
        raise ReplayError("builder.packages must record at least one exact package version")
    for index, package in enumerate(packages):
        package = _require_string(package, f"builder.packages[{index}]")
        if "=" not in package:
            raise ReplayError(f"builder.packages[{index}] must include an exact version with '='")
    for field in ("configure_argv", "build_argv"):
        values = _require_list(builder.get(field), f"builder.{field}")
        if not values:
            raise ReplayError(f"builder.{field} must not be empty")
        for index, value in enumerate(values):
            _require_string(value, f"builder.{field}[{index}]")
    toolchain = _require_string_map(builder.get("toolchain"), "builder.toolchain")
    if not toolchain:
        raise ReplayError("builder.toolchain must record the exact compiler and linker versions")

    archive = _require_dict(lock.get("archive"), "archive")
    _require_sha256(archive.get("sha256"), "archive.sha256")
    _require_int(archive.get("bytes"), "archive.bytes", minimum=1)
    url = archive.get("url")
    if url is not None:
        url = _require_string(url, "archive.url")
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or parsed.netloc != "github.com" or "/releases/download/" not in parsed.path:
            raise ReplayError("archive.url must be an immutable HTTPS GitHub Release asset URL")

    link = _require_dict(lock.get("link"), "link")
    arguments = _require_list(link.get("arguments"), "link.arguments")
    if not arguments:
        raise ReplayError("link.arguments must contain the exact captured linker argument vector")
    for index, argument in enumerate(arguments):
        _require_string(argument, f"link.arguments[{index}]", allow_empty=True)
    if "--no-fork" not in arguments:
        raise ReplayError("link.arguments must include '--no-fork' for link-process-only timing")
    if sum(argument.count(OUTPUT_TOKEN) for argument in arguments) != 1:
        raise ReplayError(f"link.arguments must contain {OUTPUT_TOKEN!r} exactly once")
    for argument in arguments:
        scrubbed = argument.replace(OUTPUT_TOKEN, "").replace(CORPUS_TOKEN, "")
        if "{" in scrubbed or "}" in scrubbed:
            raise ReplayError(f"link.arguments contains an unsupported template token: {argument!r}")
    _safe_relative_path(link.get("cwd"), "link.cwd", allow_dot=True)
    _require_string_map(link.get("environment"), "link.environment")
    _require_string(link.get("stdout_utf8"), "link.stdout_utf8", allow_empty=True)
    _require_string(link.get("stderr_utf8"), "link.stderr_utf8", allow_empty=True)

    response_files = _require_list(link.get("response_files"), "link.response_files")
    response_paths = {
        _safe_relative_path(path, f"link.response_files[{index}]")
        for index, path in enumerate(response_files)
    }
    if len(response_paths) != len(response_files):
        raise ReplayError("link.response_files contains a duplicate path")

    oracle = _require_dict(lock.get("oracle"), "oracle")
    oracle_arguments = _require_list(oracle.get("arguments"), "oracle.arguments")
    if not oracle_arguments:
        raise ReplayError("oracle.arguments must not be empty")
    for index, argument in enumerate(oracle_arguments):
        _require_string(argument, f"oracle.arguments[{index}]", allow_empty=True)
    if sum(argument.count(OUTPUT_TOKEN) for argument in oracle_arguments) != 1:
        raise ReplayError(f"oracle.arguments must contain {OUTPUT_TOKEN!r} exactly once")
    for argument in oracle_arguments:
        scrubbed = argument.replace(OUTPUT_TOKEN, "").replace(CORPUS_TOKEN, "")
        if "{" in scrubbed or "}" in scrubbed:
            raise ReplayError(f"oracle.arguments contains an unsupported template token: {argument!r}")
    _require_int(oracle.get("exit_code"), "oracle.exit_code")
    _require_string(oracle.get("stdout_utf8"), "oracle.stdout_utf8", allow_empty=True)
    _require_string(oracle.get("stderr_utf8"), "oracle.stderr_utf8", allow_empty=True)

    replay = _require_dict(lock.get("replay"), "replay")
    if replay.get("target_seconds") != TARGET_SECONDS:
        raise ReplayError(f"replay.target_seconds must be exactly {TARGET_SECONDS}")
    if replay.get("warmup_runs") != WARMUP_RUNS:
        raise ReplayError(f"replay.warmup_runs must be exactly {WARMUP_RUNS}")
    if replay.get("identity_runs_per_side") != IDENTITY_RUNS_PER_SIDE:
        raise ReplayError(
            f"replay.identity_runs_per_side must be exactly {IDENTITY_RUNS_PER_SIDE}"
        )

    files = _require_list(lock.get("files"), "files")
    if not files:
        raise ReplayError("files must contain the complete input closure")
    file_paths: set[str] = set()
    for index, raw_file in enumerate(files):
        item = _require_dict(raw_file, f"files[{index}]")
        path = _safe_relative_path(item.get("path"), f"files[{index}].path")
        if path in file_paths:
            raise ReplayError(f"files contains duplicate path {path!r}")
        file_paths.add(path)
        _require_sha256(item.get("sha256"), f"files[{index}].sha256")
        _require_int(item.get("bytes"), f"files[{index}].bytes")
    missing_responses = sorted(response_paths - file_paths)
    if missing_responses:
        raise ReplayError(f"response files are absent from the input closure: {missing_responses}")


def load_lock(path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise ReplayError(
            f"corpus lock does not exist: {path}; pass --lock for a real captured corpus"
        )
    try:
        lock = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReplayError(f"unable to read corpus lock {path}: {error}") from error
    lock = _require_dict(lock, "lock")
    validate_lock(lock)
    return lock


def _download_archive(url: str, destination: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "reld-clang-link-replay/1"})
    try:
        with urllib.request.urlopen(request) as response, destination.open("wb") as output:  # noqa: S310
            shutil.copyfileobj(response, output, length=CHUNK)
    except OSError as error:
        raise ReplayError(f"unable to download corpus archive {url}: {error}") from error


def acquire_archive(lock: dict[str, Any], *, archive_override: Path | None, destination: Path) -> Path:
    """Resolve a local override or immutable release URL, then verify size and SHA-256."""
    archive = _require_dict(lock["archive"], "archive")
    if archive_override is not None:
        source = archive_override.resolve()
        if not source.is_file():
            raise ReplayError(f"corpus archive does not exist: {source}")
    else:
        url = archive.get("url")
        if not url:
            raise ReplayError(
                "corpus archive is not published in this lock; pass --archive with the locally "
                "built archive (the lock must still contain its real SHA-256 and byte size)"
            )
        destination.parent.mkdir(parents=True, exist_ok=True)
        _download_archive(url, destination)
        source = destination

    actual_bytes = source.stat().st_size
    if actual_bytes != archive["bytes"]:
        raise ReplayError(
            f"archive byte-size mismatch for {source}: expected {archive['bytes']}, got {actual_bytes}"
        )
    actual_sha256 = sha256_file(source)
    if actual_sha256 != archive["sha256"]:
        raise ReplayError(
            f"archive SHA-256 mismatch for {source}: expected {archive['sha256']}, "
            f"got {actual_sha256}"
        )
    return source


def verify_input_closure(lock: dict[str, Any], corpus_root: Path) -> None:
    """Require every archived regular file, and no unrecorded file, to match the lock."""
    expected = {item["path"]: item for item in lock["files"]}
    actual: dict[str, Path] = {}
    for path in corpus_root.rglob("*"):
        if path.is_symlink():
            raise ReplayError(f"corpus input closure contains a symlink: {path}")
        if path.is_file():
            relative = path.relative_to(corpus_root).as_posix()
            actual[relative] = path
    missing = sorted(set(expected) - set(actual))
    extra = sorted(set(actual) - set(expected))
    if missing or extra:
        raise ReplayError(f"corpus input closure differs from lock: missing={missing}, extra={extra}")
    for relative, item in expected.items():
        path = actual[relative]
        actual_bytes = path.stat().st_size
        if actual_bytes != item["bytes"]:
            raise ReplayError(
                f"input byte-size mismatch for {relative}: expected {item['bytes']}, got {actual_bytes}"
            )
        actual_sha256 = sha256_file(path)
        if actual_sha256 != item["sha256"]:
            raise ReplayError(
                f"input SHA-256 mismatch for {relative}: expected {item['sha256']}, "
                f"got {actual_sha256}"
            )


def extract_and_verify(lock: dict[str, Any], archive: Path, corpus_root: Path) -> None:
    if corpus_root.exists() and any(corpus_root.iterdir()):
        raise ReplayError(f"refusing to extract over non-empty corpus directory: {corpus_root}")
    corpus_root.mkdir(parents=True, exist_ok=True)
    try:
        extract_archive(archive, corpus_root)
    except (OSError, subprocess.SubprocessError, SystemExit) as error:
        raise ReplayError(f"unable to extract corpus archive {archive}: {error}") from error
    verify_input_closure(lock, corpus_root)


def _expand(argument: str, *, corpus_root: Path, output: Path) -> str:
    return argument.replace(CORPUS_TOKEN, str(corpus_root)).replace(OUTPUT_TOKEN, str(output))


def link_recipe(lock: dict[str, Any]) -> LinkRecipe:
    link = lock["link"]
    return LinkRecipe(
        arguments=tuple(link["arguments"]),
        cwd=link["cwd"],
        environment=dict(link["environment"]),
        stdout=link["stdout_utf8"].encode("utf-8"),
        stderr=link["stderr_utf8"].encode("utf-8"),
    )


def native_oracle(lock: dict[str, Any]) -> NativeOracle:
    oracle = lock["oracle"]
    return NativeOracle(
        arguments=tuple(oracle["arguments"]),
        exit_code=oracle["exit_code"],
        stdout=oracle["stdout_utf8"].encode("utf-8"),
        stderr=oracle["stderr_utf8"].encode("utf-8"),
    )


def _run_native_oracle(
    output: Path,
    *,
    oracle: NativeOracle,
    corpus_root: Path,
    cwd: Path,
    environment: dict[str, str],
) -> None:
    command = [_expand(argument, corpus_root=corpus_root, output=output) for argument in oracle.arguments]
    try:
        completed = subprocess.run(command, cwd=cwd, env=environment, capture_output=True, check=False)
    except OSError as error:
        raise ReplayError(f"unable to execute native oracle for {output}: {error}") from error
    if (
        completed.returncode != oracle.exit_code
        or completed.stdout != oracle.stdout
        or completed.stderr != oracle.stderr
    ):
        raise ReplayError(
            f"native oracle mismatch for {output}: expected exit/stdout/stderr "
            f"{oracle.exit_code}/{oracle.stdout!r}/{oracle.stderr!r}, got "
            f"{completed.returncode}/{completed.stdout!r}/{completed.stderr!r}"
        )


def _link_and_validate(
    linker: Path,
    output: Path,
    *,
    recipe: LinkRecipe,
    oracle: NativeOracle,
    corpus_root: Path,
    clock: Callable[[], float] = time.perf_counter,
) -> float:
    """Run one fixed final link and exact native oracle, timing only the linker subprocess."""
    output.unlink(missing_ok=True)
    cwd = corpus_root / recipe.cwd
    arguments = [_expand(argument, corpus_root=corpus_root, output=output) for argument in recipe.arguments]
    command = [str(linker), *arguments]
    started = clock()
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=recipe.environment,
            capture_output=True,
            check=False,
        )
    except OSError as error:
        raise ReplayError(f"unable to execute linker {linker}: {error}") from error
    elapsed = clock() - started
    if completed.returncode != 0 or completed.stdout != recipe.stdout or completed.stderr != recipe.stderr:
        raise ReplayError(
            f"link failed exact exit/stdout/stderr validation for {linker}: "
            f"{completed.returncode}/{completed.stdout!r}/{completed.stderr!r}"
        )
    if not output.is_file() or output.stat().st_size == 0:
        raise ReplayError(f"linker produced no non-empty output: {output}")
    _run_native_oracle(
        output,
        oracle=oracle,
        corpus_root=corpus_root,
        cwd=cwd,
        environment=recipe.environment,
    )
    return elapsed


def first_differing_offset(left: Path, right: Path) -> int | None:
    """Return the first raw-byte difference, including the common-prefix EOF offset."""
    offset = 0
    with left.open("rb") as left_handle, right.open("rb") as right_handle:
        while True:
            left_chunk = left_handle.read(CHUNK)
            right_chunk = right_handle.read(CHUNK)
            common = min(len(left_chunk), len(right_chunk))
            for index in range(common):
                if left_chunk[index] != right_chunk[index]:
                    return offset + index
            if len(left_chunk) != len(right_chunk):
                return offset + common
            if not left_chunk:
                return None
            offset += len(left_chunk)


def _write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _identity_gate(
    baseline: Path,
    candidate: Path,
    *,
    lock: dict[str, Any],
    corpus_root: Path,
    output: Path,
    artifact_dir: Path,
) -> dict[str, Any]:
    recipe = link_recipe(lock)
    oracle = native_oracle(lock)
    artifacts: list[Path] = []
    artifact_dir.mkdir(parents=True, exist_ok=True)
    for label, linker in (("baseline", baseline), ("candidate", candidate)):
        for run in range(1, IDENTITY_RUNS_PER_SIDE + 1):
            _link_and_validate(
                linker,
                output,
                recipe=recipe,
                oracle=oracle,
                corpus_root=corpus_root,
            )
            retained = artifact_dir / f"{label}-{run}"
            shutil.copyfile(output, retained)
            artifacts.append(retained)

    reference = artifacts[0]
    hashes = {path.name: sha256_file(path) for path in artifacts}
    for contender in artifacts[1:]:
        difference = first_differing_offset(reference, contender)
        if difference is not None:
            failure = {
                "status": "failed",
                "reason": "raw artifact mismatch",
                "reference": str(reference),
                "contender": str(contender),
                "first_differing_offset": difference,
                "sha256": hashes,
            }
            _write_json(artifact_dir / "identity-failure.json", failure)
            raise ReplayError(
                f"raw artifact mismatch at offset {difference}: {reference} vs {contender}; "
                f"all four artifacts and {artifact_dir / 'identity-failure.json'} were retained"
            )
    return {"status": "passed", "sha256": hashes, "artifacts": [str(path) for path in artifacts]}


def run_replay(
    lock: dict[str, Any],
    *,
    baseline: Path,
    candidate: Path,
    corpus_root: Path,
    workdir: Path,
    report_path: Path,
    max_replays: int = 10_000,
) -> dict[str, Any]:
    """Gate two baseline/two candidate outputs, then time ~30 seconds of candidate links."""
    validate_lock(lock)
    if sys.platform != "linux":
        raise ReplayError("the immutable Clang corpus replay is Linux-only")
    for label, linker in (("baseline", baseline), ("candidate", candidate)):
        if not linker.is_file():
            raise ReplayError(f"{label} linker does not exist: {linker}")
    if max_replays < 1:
        raise ReplayError("max_replays must be positive")
    verify_input_closure(lock, corpus_root)

    output_dir = workdir / "output"
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / "clang"
    identity = _identity_gate(
        baseline,
        candidate,
        lock=lock,
        corpus_root=corpus_root,
        output=output,
        artifact_dir=workdir / "identity-artifacts",
    )

    recipe = link_recipe(lock)
    oracle = native_oracle(lock)
    calibration_seconds = _link_and_validate(
        candidate,
        output,
        recipe=recipe,
        oracle=oracle,
        corpus_root=corpus_root,
    )
    if not math.isfinite(calibration_seconds) or calibration_seconds <= 0:
        raise ReplayError(f"invalid warmup calibration duration: {calibration_seconds}")
    replay_count = max(1, round(TARGET_SECONDS / calibration_seconds))
    if replay_count > max_replays:
        raise ReplayError(
            f"calibration requested {replay_count} replays, exceeding --max-replays={max_replays}"
        )

    samples: list[float] = []
    for _ in range(replay_count):
        samples.append(
            _link_and_validate(
                candidate,
                output,
                recipe=recipe,
                oracle=oracle,
                corpus_root=corpus_root,
            )
        )
    report: dict[str, Any] = {
        "schema_version": 1,
        "status": "passed",
        "timing_scope": "final linker subprocess only",
        "target_seconds": TARGET_SECONDS,
        "warmup_runs": WARMUP_RUNS,
        "calibration_seconds": calibration_seconds,
        "fixed_replay_count": replay_count,
        "sample_seconds": samples,
        "timed_total_seconds": sum(samples),
        "identity": identity,
        "baseline": {"path": str(baseline), "sha256": sha256_file(baseline)},
        "candidate": {"path": str(candidate), "sha256": sha256_file(candidate)},
        "corpus_archive_sha256": lock["archive"]["sha256"],
        "source": lock["source"],
        "builder": lock["builder"],
        "link_arguments": list(recipe.arguments),
        "link_environment": recipe.environment,
        "native_oracle_after_every_link": True,
        "runner": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": sys.version,
        },
        "completed_at": datetime.now(timezone.utc).isoformat(),
    }
    _write_json(report_path, report)
    return report


def _cmd_validate(args: argparse.Namespace) -> int:
    lock = load_lock(args.lock)
    print(
        f"valid Clang corpus lock: {args.lock} "
        f"({len(lock['files'])} files, archive {lock['archive']['sha256']})"
    )
    return 0


def _cmd_replay(args: argparse.Namespace) -> int:
    lock = load_lock(args.lock)
    workdir = args.workdir.resolve()
    corpus_root = workdir / "corpus"
    archive_url = lock["archive"].get("url")
    archive_name = (
        Path(urllib.parse.urlparse(archive_url).path).name
        if archive_url
        else "clang-link-corpus.tar.zst"
    )
    archive = acquire_archive(
        lock,
        archive_override=args.archive,
        destination=workdir / "download" / archive_name,
    )
    extract_and_verify(lock, archive, corpus_root)
    report = run_replay(
        lock,
        baseline=args.baseline.resolve(),
        candidate=args.candidate.resolve(),
        corpus_root=corpus_root,
        workdir=workdir,
        report_path=args.report.resolve(),
        max_replays=args.max_replays,
    )
    print(
        f"artifact gate passed; {report['fixed_replay_count']} link-only samples totaled "
        f"{report['timed_total_seconds']:.3f}s; evidence: {args.report}"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate-lock")
    validate.add_argument("--lock", type=Path, required=True)
    validate.set_defaults(func=_cmd_validate)

    replay = subparsers.add_parser("replay")
    replay.add_argument("--lock", type=Path, required=True)
    replay.add_argument(
        "--archive",
        type=Path,
        help="local archive override; required while lock.archive.url is unpublished",
    )
    replay.add_argument("--baseline", type=Path, required=True)
    replay.add_argument("--candidate", type=Path, required=True)
    replay.add_argument("--workdir", type=Path, required=True)
    replay.add_argument("--report", type=Path, required=True)
    replay.add_argument("--max-replays", type=int, default=10_000)
    replay.set_defaults(func=_cmd_replay)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except ReplayError as error:
        parser.exit(1, f"clang link replay failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
