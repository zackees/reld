"""Real-world link corpus, published as a pass rate (reld#185, sub-issue of #6 P2-T3).

Every other reld check works on tiny fixtures. This one downloads a pinned set of real C, C++,
and Rust projects (`corpus.lock.json`), links each one with reld instead of the host linker, and
runs its own smoke check. There are exactly three outcomes per project:

    pass           reld linked it and the project's own smoke check passed.
    fail           reld could not link it (or the link mis-behaved), but the *same* project links
                   fine with the host toolchain's default linker (the "control" run) — this is a
                   real reld bug.
    harness_error  the corpus harness itself could not get a verdict: the download or checksum
                   failed, reld ran but something else silently linked the artifact instead
                   (`verify`), or the project does not even build without reld (the control also
                   failed). None of these say anything about reld, so they are kept out of the
                   rate rather than counted as either a pass or a fail.

The published number is `pass / (pass + fail)`: a failing project lowers the rate, but it never
fails the job — a large corpus with a slow long tail of upstream breakage must stay useful. A
harness error is reported (and, in CI, annotated) separately so it does not silently move the
rate in either direction.

This module has no credentials and never touches git: it measures and writes a JSON report plus
a Markdown summary to `--output-dir`. Publishing that report (the credentialed push of `corpus/`
onto the benchmark-stats branch) stays in `.github/workflows/corpus.yml`, where the token is.

The manifest ships with `sha256: "UNPINNED"` placeholders only until the first
`python -m ci.corpus --pin` run fills them from real downloads; `load_manifest` refuses to
measure an unpinned project.

Local use, against a freshly built reld:

    uv run --no-sync python -m ci.corpus --reld "$PWD/target/release/reld" \\
        --workdir target/corpus --publish --output-dir target/corpus-output
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence
from urllib.request import urlopen

from ci.benchmark_assets import sha256_file
from ci.selfhost import section_range

SCHEMA_VERSION = 1
DEFAULT_MANIFEST = Path(__file__).with_name("corpus.lock.json")
UNPINNED = "UNPINNED"

PASS = "pass"
FAIL = "fail"
HARNESS_ERROR = "harness_error"

#: reld writes "Reld <version> (compatible with GNU linkers)" into .comment (see
#: crates/reld-core/src/args.rs `linker_identity`); this is how `linked_by_reld` tells a real
#: reld link apart from one where something else on PATH silently did the linking instead.
RELD_IDENTITY = b"Reld "

#: Report rows are read as a table; a multi-kilobyte build log in one cell defeats that.
FIRST_LINE_LIMIT = 300


class CorpusError(RuntimeError):
    """A bad manifest or missing input. Fails the whole run before any project is measured."""


class FetchError(RuntimeError):
    """A download/checksum/extract problem for one project. Becomes a harness_error row."""


@dataclass(frozen=True)
class Project:
    name: str
    language: str
    stressors: tuple[str, ...]
    revision: str
    url: str
    sha256: str
    archive_root: str
    ldflags: tuple[str, ...]
    build: tuple[tuple[str, ...], ...]
    smoke: tuple[tuple[str, ...], ...]
    artifacts: tuple[str, ...]
    timeout_seconds: int


@dataclass(frozen=True)
class Outcome:
    ok: bool
    stage: str | None
    output: str
    returncode: int | None = None


@dataclass(frozen=True)
class ProjectResult:
    name: str
    language: str
    stressors: tuple[str, ...]
    revision: str
    url: str
    sha256: str
    status: str
    stage: str | None
    first_line: str | None
    control: str
    seconds: float

    def to_json(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "language": self.language,
            "stressors": list(self.stressors),
            "revision": self.revision,
            "url": self.url,
            "sha256": self.sha256,
            "status": self.status,
            "stage": self.stage,
            "first_line": self.first_line,
            "control": self.control,
            "seconds": round(self.seconds, 1),
        }


_EXPECTED_PROJECT_KEYS = {
    "name",
    "language",
    "stressors",
    "revision",
    "url",
    "sha256",
    "archive_root",
    "ldflags",
    "build",
    "smoke",
    "artifacts",
    "timeout_seconds",
}


def _require_argv_list(project_name: str, field_name: str, value: Any) -> tuple[tuple[str, ...], ...]:
    if not isinstance(value, list) or not value:
        raise CorpusError(f"{project_name}: {field_name} must be a non-empty list")
    argvs: list[tuple[str, ...]] = []
    for argv in value:
        if not isinstance(argv, list) or not argv or not all(isinstance(part, str) for part in argv):
            raise CorpusError(f"{project_name}: {field_name} entries must be non-empty lists of strings")
        argvs.append(tuple(argv))
    return tuple(argvs)


def load_manifest(path: Path, *, allow_unpinned: bool = False) -> list[Project]:
    """Parse and validate `corpus.lock.json`, naming the offending project in every error."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise CorpusError(f"cannot read manifest {path}: {error}") from error
    try:
        data = json.loads(text)
    except json.JSONDecodeError as error:
        raise CorpusError(f"{path} is not valid JSON: {error}") from error

    if not isinstance(data, dict) or data.get("schema_version") != SCHEMA_VERSION:
        raise CorpusError(f"{path}: schema_version must be {SCHEMA_VERSION}")
    entries = data.get("projects")
    if not isinstance(entries, list):
        raise CorpusError(f"{path}: manifest has no 'projects' list")

    projects: list[Project] = []
    seen_names: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise CorpusError(f"{path}: project entry must be a JSON object, got {entry!r}")
        raw_name = entry.get("name")
        label = raw_name if isinstance(raw_name, str) and raw_name else "<unnamed project>"

        keys = set(entry.keys())
        missing = _EXPECTED_PROJECT_KEYS - keys
        if missing:
            raise CorpusError(f"{label}: manifest entry missing key(s) {sorted(missing)}")
        extra = keys - _EXPECTED_PROJECT_KEYS
        if extra:
            raise CorpusError(f"{label}: manifest entry has unexpected key(s) {sorted(extra)}")

        name = entry["name"]
        if not isinstance(name, str) or not name:
            raise CorpusError(f"{label}: name must be a non-empty string")
        if name in seen_names:
            raise CorpusError(f"{name}: duplicate project name")
        seen_names.add(name)

        language = entry["language"]
        if language not in ("c", "c++", "rust"):
            raise CorpusError(f"{name}: language must be one of c, c++, rust, got {language!r}")

        stressors = entry["stressors"]
        if not isinstance(stressors, list) or not all(isinstance(s, str) for s in stressors):
            raise CorpusError(f"{name}: stressors must be a list of strings")

        revision = entry["revision"]
        if not isinstance(revision, str) or not revision:
            raise CorpusError(f"{name}: revision must be a non-empty string")

        url = entry["url"]
        if not isinstance(url, str) or not url.startswith("https://"):
            raise CorpusError(f"{name}: url must start with https://, got {url!r}")

        sha256 = entry["sha256"]
        if not isinstance(sha256, str):
            raise CorpusError(f"{name}: sha256 must be a string")
        if sha256 == UNPINNED:
            if not allow_unpinned:
                raise CorpusError(f"{name}: sha256 is UNPINNED; run `python -m ci.corpus --pin` first")
        elif not re.fullmatch(r"[0-9a-f]{64}", sha256):
            raise CorpusError(f"{name}: sha256 must be 64 lowercase hex characters, got {sha256!r}")

        archive_root = entry["archive_root"]
        if not isinstance(archive_root, str) or not archive_root:
            raise CorpusError(f"{name}: archive_root must be a non-empty string")

        ldflags = entry["ldflags"]
        if not isinstance(ldflags, list) or not all(isinstance(f, str) for f in ldflags):
            raise CorpusError(f"{name}: ldflags must be a list of strings")

        build = _require_argv_list(name, "build", entry["build"])
        smoke = _require_argv_list(name, "smoke", entry["smoke"])

        raw_artifacts = entry["artifacts"]
        if not isinstance(raw_artifacts, list) or not raw_artifacts:
            raise CorpusError(f"{name}: artifacts must be a non-empty list")
        artifacts: list[str] = []
        for artifact in raw_artifacts:
            if not isinstance(artifact, str) or not artifact:
                raise CorpusError(f"{name}: artifacts entries must be non-empty strings")
            parts = PurePosixPath(artifact)
            if parts.is_absolute() or ".." in parts.parts:
                raise CorpusError(f"{name}: artifact path must be relative and not contain '..': {artifact}")
            artifacts.append(artifact)

        timeout_seconds = entry["timeout_seconds"]
        if isinstance(timeout_seconds, bool) or not isinstance(timeout_seconds, int) or timeout_seconds <= 0:
            raise CorpusError(f"{name}: timeout_seconds must be a positive integer, got {timeout_seconds!r}")

        projects.append(
            Project(
                name=name,
                language=language,
                stressors=tuple(stressors),
                revision=revision,
                url=url,
                sha256=sha256,
                archive_root=archive_root,
                ldflags=tuple(ldflags),
                build=build,
                smoke=smoke,
                artifacts=tuple(artifacts),
                timeout_seconds=timeout_seconds,
            )
        )
    return projects


def first_failure_line(output: str, returncode: int | None = None) -> str:
    """The line that best explains a failure: the first mention of "error", else the tail."""
    lines = output.splitlines()
    for line in lines:
        stripped = line.strip()
        if stripped and re.search(r"\berror\b", stripped, re.IGNORECASE):
            return stripped[:FIRST_LINE_LIMIT]
    for line in reversed(lines):
        stripped = line.strip()
        if stripped:
            return stripped[:FIRST_LINE_LIMIT]
    if returncode is None:
        return "no output"
    return f"exit {returncode}"[:FIRST_LINE_LIMIT]


def classify(reld: Outcome, control: Outcome | None) -> tuple[str, str | None, str | None]:
    """Decide pass/fail/harness_error from the reld attempt and (maybe) the control attempt.

    `verify` failing (reld ran, but the artifact was not actually linked by reld) is always a
    harness defect, never a reld bug, regardless of whether the control passes.
    """
    if reld.ok:
        return PASS, None, None
    if reld.stage in ("fetch", "verify", "harness"):
        return HARNESS_ERROR, reld.stage, first_failure_line(reld.output, reld.returncode)
    if control is None:
        raise ValueError(f"reld failed at stage {reld.stage!r} with no control outcome to compare against")
    if control.ok:
        return FAIL, reld.stage, first_failure_line(reld.output, reld.returncode)
    message = "fails without reld too: " + first_failure_line(control.output, control.returncode)
    return HARNESS_ERROR, control.stage, message[:FIRST_LINE_LIMIT]


def pass_rate(results: Sequence[ProjectResult]) -> float | None:
    """passes / (passes + fails); harness errors are excluded from both sides of the ratio."""
    passes = sum(1 for r in results if r.status == PASS)
    fails = sum(1 for r in results if r.status == FAIL)
    denominator = passes + fails
    if denominator == 0:
        return None
    return passes / denominator


def build_report(
    results: Sequence[ProjectResult],
    *,
    reld_commit: str,
    reld_version: str,
    runner: dict[str, Any],
    generated_at: str,
) -> dict[str, Any]:
    passes = sum(1 for r in results if r.status == PASS)
    fails = sum(1 for r in results if r.status == FAIL)
    harness_errors = sum(1 for r in results if r.status == HARNESS_ERROR)
    rate = pass_rate(results)
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": generated_at,
        "reld": {"commit": reld_commit, "version": reld_version},
        "runner": runner,
        "totals": {
            "projects": len(results),
            "pass": passes,
            "fail": fails,
            "harness_error": harness_errors,
        },
        "pass_rate": round(rate, 4) if rate is not None else None,
        "projects": [r.to_json() for r in results],
    }


def render_summary(report: dict[str, Any]) -> str:
    totals = report["totals"]
    rate = report["pass_rate"]
    lines = ["### reld link corpus", ""]
    if rate is None:
        lines.append("pass rate: n/a (no project measured reld)")
    else:
        measured = totals["pass"] + totals["fail"]
        lines.append(
            f"pass rate: {totals['pass']}/{measured} ({rate * 100:.1f}%); "
            f"{totals['harness_error']} harness error(s) excluded from the rate"
        )
    lines.append("")
    lines.append(f"reld commit `{report['reld']['commit']}` ({report['reld']['version']}) on runner `{report['runner'].get('name', '')}`")
    lines.append("")
    lines.append("| project | language | stressors | revision | status | stage | first line |")
    lines.append("|---|---|---|---|---|---|---|")
    for project in report["projects"]:
        first_line = (project["first_line"] or "").replace("|", "\\|")
        lines.append(
            "| {name} | {language} | {stressors} | {revision} | {status} | {stage} | {first_line} |".format(
                name=project["name"],
                language=project["language"],
                stressors=", ".join(project["stressors"]),
                revision=project["revision"],
                status=project["status"],
                stage=project["stage"] or "",
                first_line=first_line,
            )
        )
    return "\n".join(lines) + "\n"


def fetch(project: Project, downloads: Path, *, opener: Any = urllib.request.urlopen) -> Path:
    """Download and checksum-verify one project's archive, reusing a cached copy if it matches."""
    downloads.mkdir(parents=True, exist_ok=True)
    basename = project.url.rsplit("/", 1)[-1]
    target = downloads / f"{project.sha256}-{basename}"
    if target.is_file() and sha256_file(target) == project.sha256:
        return target

    part = target.with_name(target.name + ".part")
    try:
        with opener(project.url, timeout=120) as response:
            part.write_bytes(response.read())
    except (urllib.error.URLError, OSError, ValueError) as error:
        part.unlink(missing_ok=True)
        raise FetchError(f"{project.name}: failed to download {project.url}: {error}") from error

    digest = sha256_file(part)
    if digest != project.sha256:
        part.unlink(missing_ok=True)
        raise FetchError(f"{project.name}: sha256 mismatch for {project.url}: expected {project.sha256}, got {digest}")
    part.replace(target)
    return target


def _safe_extract_tar(tar: tarfile.TarFile, dest: Path) -> None:
    try:
        tar.extractall(dest, filter="data")  # noqa: S202 - "data" filter rejects unsafe members
        return
    except TypeError:
        pass  # Python < 3.12: no `filter` kwarg; fall back to a manual check below.
    for member in tar.getmembers():
        parts = PurePosixPath(member.name)
        if parts.is_absolute() or ".." in parts.parts:
            raise FetchError(f"unsafe path in archive: {member.name}")
    tar.extractall(dest)  # noqa: S202 - members validated above


def extract(archive: Path, dest: Path, archive_root: str) -> Path:
    """Extract `archive` fresh into `dest` and return `dest / archive_root`."""
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(dest)
    elif archive.name.endswith((".tar.gz", ".tgz", ".crate")):
        with tarfile.open(archive) as tar:
            _safe_extract_tar(tar, dest)
    else:
        raise FetchError(f"{archive}: unsupported archive extension")

    root = dest / archive_root
    if not root.is_dir():
        raise FetchError(f"{archive}: extracted archive has no top-level directory {archive_root!r}")
    return root


def run_command(argv: Sequence[str], cwd: Path, env: dict[str, str], timeout: int) -> tuple[int, str]:
    try:
        result = subprocess.run(
            list(argv),
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return 124, f"timed out after {timeout}s"
    except FileNotFoundError:
        return 127, f"{argv[0]}: command not found"
    return result.returncode, result.stdout


def attempt_env(
    project: Project,
    linker: Path | None,
    *,
    cc: str,
    cxx: str,
    base_env: Mapping[str, str],
    root: Path,
) -> dict[str, str]:
    """Build the environment for one attempt. `linker` is None for the host-default control."""
    env = dict(base_env)
    env["CC"] = cc
    env["CXX"] = cxx
    env["JOBS"] = str(os.cpu_count() or 2)
    ld_flags = ([f"--ld-path={linker}"] if linker else []) + list(project.ldflags)
    env["LDFLAGS"] = " ".join(ld_flags)
    if project.language == "rust":
        env["RUSTFLAGS"] = f"-C linker={cc} -C link-arg=--ld-path={linker}" if linker else f"-C linker={cc}"
        env["CARGO_TARGET_DIR"] = str(root / "target")
    return env


def linked_by_reld(path: Path) -> bool:
    """Whether `path`'s `.comment` section carries reld's own identity string.

    This is how a `verify` failure is told apart from a real link: the build reported success,
    but something other than reld (a stale cached binary, a fallback linker) produced the file.
    """
    resolved = path.resolve()
    try:
        span = section_range(resolved, b".comment")
    except (RuntimeError, OSError, struct.error):
        # Not a 64-bit ELF (SelfHostError), unreadable, or a truncated section table.
        return False
    if span is None:
        return False
    start, stop = span
    with resolved.open("rb") as handle:
        handle.seek(start)
        data = handle.read(stop - start)
    return RELD_IDENTITY in data


def attempt(
    project: Project,
    archive: Path,
    workdir: Path,
    linker: Path | None,
    *,
    cc: str,
    cxx: str,
    runner: Any = run_command,
) -> Outcome:
    """Extract fresh, build, verify (when linked by reld), then smoke-test."""
    source_root = extract(archive, workdir, project.archive_root)
    # CARGO_TARGET_DIR must sit under the source root: artifacts are resolved relative to it.
    env = attempt_env(project, linker, cc=cc, cxx=cxx, base_env=os.environ, root=source_root)

    output_parts: list[str] = []
    for argv in project.build:
        code, output = runner(argv, source_root, env, project.timeout_seconds)
        output_parts.append(output)
        if code != 0:
            return Outcome(False, "build", "\n".join(output_parts), code)

    if linker is not None:
        for artifact in project.artifacts:
            artifact_path = source_root / artifact
            if not artifact_path.is_file():
                return Outcome(False, "verify", f"{artifact}: build produced no such file")
            if not linked_by_reld(artifact_path):
                return Outcome(
                    False,
                    "verify",
                    f"{artifact}: .comment has no reld identity; reld did not perform this link",
                )

    for argv in project.smoke:
        code, output = runner(argv, source_root, env, project.timeout_seconds)
        output_parts.append(output)
        if code != 0:
            return Outcome(False, "smoke", "\n".join(output_parts), code)

    return Outcome(True, None, "")


def _finish(
    project: Project,
    reld_outcome: Outcome,
    control_outcome: Outcome | None,
    control: str,
    start: float,
) -> ProjectResult:
    status, stage, first_line = classify(reld_outcome, control_outcome)
    return ProjectResult(
        name=project.name,
        language=project.language,
        stressors=project.stressors,
        revision=project.revision,
        url=project.url,
        sha256=project.sha256,
        status=status,
        stage=stage,
        first_line=first_line,
        control=control,
        seconds=time.monotonic() - start,
    )


def run_project(
    project: Project,
    reld: Path,
    workdir: Path,
    downloads: Path,
    *,
    cc: str = "clang",
    cxx: str = "clang++",
    runner: Any = run_command,
    opener: Any = urllib.request.urlopen,
) -> ProjectResult:
    """Measure one project end to end. Never raises: every failure becomes a `ProjectResult`."""
    start = time.monotonic()
    control = "not_run"

    try:
        archive = fetch(project, downloads, opener=opener)
    except FetchError as error:
        return _finish(project, Outcome(False, "fetch", str(error)), None, control, start)

    control_outcome: Outcome | None = None
    try:
        try:
            reld_outcome = attempt(
                project, archive, workdir / project.name / "reld", reld, cc=cc, cxx=cxx, runner=runner
            )
        except FetchError as error:
            reld_outcome = Outcome(False, "fetch", str(error))

        if not reld_outcome.ok and reld_outcome.stage in ("build", "smoke"):
            control_outcome = attempt(
                project, archive, workdir / project.name / "control", None, cc=cc, cxx=cxx, runner=runner
            )
            control = PASS if control_outcome.ok else FAIL
    except Exception as error:  # noqa: BLE001 - one project's bug must not fail the whole run
        reld_outcome = Outcome(False, "harness", f"{type(error).__name__}: {error}")
        control_outcome = None
        control = "not_run"

    return _finish(project, reld_outcome, control_outcome, control, start)


def _first_line_of_version(argv: Sequence[str]) -> str:
    try:
        result = subprocess.run(list(argv), capture_output=True, text=True, check=False)
    except (OSError, subprocess.SubprocessError):
        return "unknown"
    if result.returncode != 0:
        return "unknown"
    for line in result.stdout.splitlines():
        stripped = line.strip()
        if stripped:
            return stripped
    return "unknown"


def runner_info(cc: str) -> dict[str, str]:
    image_os = os.environ.get("ImageOS", "")
    image_version = os.environ.get("ImageVersion", "")
    return {
        "name": os.environ.get("RUNNER_NAME") or platform.node(),
        "os": platform.system(),
        "arch": platform.machine(),
        "image": f"{image_os} {image_version}".strip(),
        "cc": _first_line_of_version([cc, "--version"]),
        "rustc": _first_line_of_version(["rustc", "--version"]),
    }


def reld_identity(reld: Path) -> tuple[str, str]:
    commit = os.environ.get("GITHUB_SHA")
    if not commit:
        repo_root = Path(__file__).parents[1]
        try:
            result = subprocess.run(
                ["git", "-C", str(repo_root), "rev-parse", "HEAD"],
                capture_output=True,
                text=True,
                check=False,
            )
            commit = result.stdout.strip() if result.returncode == 0 else None
        except OSError:
            commit = None
    version = _first_line_of_version([str(reld), "--version"])
    return commit or "unknown", version


def pin(manifest: Path, downloads: Path, *, opener: Any = urlopen) -> list[str]:
    """Fill in every UNPINNED sha256 from a real download. Never silently repins a mismatch."""
    data = json.loads(manifest.read_text(encoding="utf-8"))
    downloads.mkdir(parents=True, exist_ok=True)

    digests_by_url: dict[str, str] = {}
    notes: list[str] = []
    for entry in data.get("projects", []):
        name = entry["name"]
        url = entry["url"]
        if url not in digests_by_url:
            basename = url.rsplit("/", 1)[-1]
            target = downloads / basename
            try:
                with opener(url, timeout=120) as response:
                    target.write_bytes(response.read())
            except (urllib.error.URLError, OSError, ValueError) as error:
                raise FetchError(f"{name}: failed to download {url}: {error}") from error
            digests_by_url[url] = sha256_file(target)
        digest = digests_by_url[url]

        current = entry.get("sha256")
        if current == UNPINNED:
            entry["sha256"] = digest
            notes.append(f"{name}: pinned sha256 to {digest}")
        elif current != digest:
            raise CorpusError(
                f"{name}: manifest sha256 {current} does not match downloaded {digest} for {url}; refusing to repin"
            )
        else:
            notes.append(f"{name}: already pinned to {digest}")

    manifest.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return notes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reld", type=Path, help="path to the reld binary under test (required unless --pin)")
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--workdir", type=Path, default=Path("target/corpus"))
    parser.add_argument("--only", action="append", default=[], metavar="NAME", help="measure only this project (repeatable)")
    parser.add_argument("--publish", action="store_true", help="write corpus.json and summary.md to --output-dir")
    parser.add_argument("--output-dir", type=Path, default=Path("corpus-output"))
    parser.add_argument("--cc", default="clang")
    parser.add_argument("--cxx", default="clang++")
    parser.add_argument("--pin", action="store_true", help="fill in UNPINNED sha256 entries and exit")
    args = parser.parse_args(argv)

    if args.pin:
        try:
            notes = pin(args.manifest, args.workdir / "downloads")
        except (CorpusError, FetchError) as error:
            sys.stderr.write(f"corpus: {error}\n")
            return 1
        for note in notes:
            print(note)
        return 0

    if args.reld is None:
        sys.stderr.write("corpus: --reld is required unless --pin is given\n")
        return 1
    reld = args.reld.resolve()
    if not (reld.is_file() and os.access(reld, os.X_OK)):
        sys.stderr.write(f"corpus: reld linker not found at {args.reld} (resolved: {reld})\n")
        return 1

    try:
        projects = load_manifest(args.manifest)
        if args.only:
            known = {p.name for p in projects}
            unknown = sorted(set(args.only) - known)
            if unknown:
                raise CorpusError(f"unknown --only project(s): {', '.join(unknown)}")
            wanted = set(args.only)
            projects = [p for p in projects if p.name in wanted]
    except CorpusError as error:
        sys.stderr.write(f"corpus: {error}\n")
        return 1

    downloads = args.workdir / "downloads"
    results: list[ProjectResult] = []
    on_ci = os.environ.get("GITHUB_ACTIONS") == "true"
    for project in projects:
        result = run_project(project, reld, args.workdir, downloads, cc=args.cc, cxx=args.cxx)
        results.append(result)
        print(f"{result.name}: {result.status} [{result.stage or ''}] {result.first_line or ''}")
        if on_ci and result.status == HARNESS_ERROR:
            print(f"::warning title=corpus harness error::{result.name}: {result.first_line}")
        elif on_ci and result.status == FAIL:
            print(f"::notice title=corpus link failure::{result.name}: {result.first_line}")

    generated_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    commit, version = reld_identity(reld)
    report = build_report(
        results,
        reld_commit=commit,
        reld_version=version,
        runner=runner_info(args.cc),
        generated_at=generated_at,
    )
    summary = render_summary(report)
    print(summary)

    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as handle:
            handle.write(summary)

    if args.publish:
        args.output_dir.mkdir(parents=True, exist_ok=True)
        (args.output_dir / "corpus.json").write_text(json.dumps(report, indent=2, sort_keys=False) + "\n", encoding="utf-8")
        (args.output_dir / "summary.md").write_text(summary, encoding="utf-8")

    measured = report["totals"]["pass"] + report["totals"]["fail"]
    if measured == 0:
        sys.stderr.write("corpus: no project measured reld; every project was a harness error\n")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
