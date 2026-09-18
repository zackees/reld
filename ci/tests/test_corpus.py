"""Per-PR unit coverage for the reld link corpus gate (`ci/corpus.py`).

The corpus itself fetches, builds and smoke-tests real upstream projects under reld and under a
`clang`-only control -- network, a toolchain, and minutes per run. AGENTS.md's "Test layout" rule
5 is explicit about that shape: a gate whose cost is gigabytes or minutes runs on a schedule and
keeps its own logic unit tested per PR, rather than slowing every push. This file is that per-PR
coverage: manifest loading and validation, pass/fail/harness_error classification, first-failure
extraction, pass-rate and report rendering, and environment construction, all exercised with fakes
-- no network, no clang, no cargo.

It is a new file, not an addition to an existing one, because the corpus gate has no existing home
in `ci/tests/`: it is not benchmarking, not a workspace-layout check, and not a workflow-only test.
"""

from __future__ import annotations

import hashlib
import io
import json
import re
import tarfile
import urllib.error
from pathlib import Path

import pytest

from ci import corpus

WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "corpus.yml"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")

NOISY_BUILD_OUTPUT = (
    "checking build system...\n"
    "make: Entering directory '/build'\n"
    "cc -c foo.c -o foo.o\n"
    "ld.reld: error: undefined symbol: foo\n"
    "make: *** [Makefile:10: foo] Error 1\n"
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _valid_project_dict(name: str = "proj", sha256: str = "a" * 64, **overrides) -> dict:
    project = {
        "name": name,
        "language": "c",
        "stressors": ["static"],
        "revision": "v1",
        "url": "https://example.invalid/proj.tar.gz",
        "sha256": sha256,
        "archive_root": "proj-1",
        "ldflags": [],
        "build": [["make"]],
        "smoke": [["./proj", "--version"]],
        "artifacts": ["proj"],
        "timeout_seconds": 600,
    }
    project.update(overrides)
    return project


def _manifest_dict(*projects: dict, schema_version: int = 1) -> dict:
    return {"schema_version": schema_version, "projects": list(projects)}


def _project(**overrides) -> corpus.Project:
    fields = dict(
        name="proj",
        language="c",
        stressors=("static",),
        revision="v1",
        url="https://example.invalid/proj.tar.gz",
        sha256="a" * 64,
        archive_root="proj-1",
        ldflags=(),
        build=(("make",),),
        smoke=(("./proj",),),
        artifacts=("proj",),
        timeout_seconds=600,
    )
    fields.update(overrides)
    return corpus.Project(**fields)


def _result(*, status: str, name: str = "proj", stage=None, first_line=None, control=None, seconds: float = 1.5):
    return corpus.ProjectResult(
        name=name,
        language="c",
        stressors=("static",),
        revision="v1",
        url="https://example.invalid/proj.tar.gz",
        sha256="a" * 64,
        status=status,
        stage=stage,
        first_line=first_line,
        control=control,
        seconds=seconds,
    )


def _make_tarball(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))


def _local_tarball(tmp_path: Path) -> Path:
    tarball = tmp_path / "src.tar.gz"
    _make_tarball(tarball, {"proj/README": b"hello"})
    return tarball


def _e2e_setup(tmp_path: Path):
    """A `Project` whose url resolves via a local-file opener, plus that opener."""
    tarball = _local_tarball(tmp_path)
    digest = hashlib.sha256(tarball.read_bytes()).hexdigest()
    project = _project(
        name="e2e-proj",
        url="https://example.invalid/e2e-proj.tar.gz",
        sha256=digest,
        archive_root="proj",
        build=(("build",),),
        smoke=(("smoke",),),
        artifacts=("out",),
    )

    def opener(url, timeout):
        return tarball.open("rb")

    return project, opener


def _runner_build_fails_only_with_reld(argv, cwd, env, timeout):
    if list(argv) == ["build"] and "--ld-path" in env.get("LDFLAGS", ""):
        return 1, "cc: undefined reference\nld.reld: error: undefined symbol: foo\n"
    Path(cwd, "out").write_text("binary")
    return 0, "ok"


def _runner_always_fails(argv, cwd, env, timeout):
    return 1, "cc: fatal error: something is broken\n"


def _runner_always_succeeds(argv, cwd, env, timeout):
    Path(cwd, "out").write_text("binary")
    return 0, "ok"


def _canned_result(name: str, status: str) -> corpus.ProjectResult:
    if status == corpus.PASS:
        stage, first_line, control = None, None, None
    elif status == corpus.FAIL:
        stage, first_line, control = "build", "ld.reld: error: x", "pass"
    else:
        stage, first_line, control = "fetch", "sha256 mismatch", None
    return corpus.ProjectResult(
        name=name,
        language="c",
        stressors=("static",),
        revision="v1",
        url=f"https://example.invalid/{name}.tar.gz",
        sha256="a" * 64,
        status=status,
        stage=stage,
        first_line=first_line,
        control=control,
        seconds=1.0,
    )


# ---------------------------------------------------------------------------
# 1. The real manifest is actually pinned
# ---------------------------------------------------------------------------


def test_real_manifest_pins_every_project() -> None:
    projects = corpus.load_manifest(corpus.DEFAULT_MANIFEST)

    assert len(projects) >= 5, f"only {len(projects)} projects in {corpus.DEFAULT_MANIFEST}"

    names = [p.name for p in projects]
    assert len(names) == len(set(names)), f"duplicate project name among {names}"

    languages = {p.language for p in projects}
    assert {"c", "c++", "rust"} <= languages, f"corpus is missing a language, got {languages}"

    stressors = {stressor for p in projects for stressor in p.stressors}
    required = {"static", "tls", "shared-library", "build-scripts", "templates"}
    missing = required - stressors
    assert not missing, f"corpus is missing issue #6 stressors: {missing}"

    for project in projects:
        assert project.revision, f"{project.name}: empty revision"
        assert SHA256_RE.match(project.sha256), f"{project.name}: sha256 is not a pinned digest ({project.sha256!r})"
        assert project.url.startswith("https://"), f"{project.name}: url is not https ({project.url!r})"


# ---------------------------------------------------------------------------
# 2. Manifest loading rejects malformed projects, by field
# ---------------------------------------------------------------------------


def _mutate_unpinned_sha(manifest: dict) -> dict:
    manifest["projects"][0]["sha256"] = corpus.UNPINNED
    return manifest


def _mutate_bad_sha_hex(manifest: dict) -> dict:
    manifest["projects"][0]["sha256"] = "abc"
    return manifest


def _mutate_bad_url_scheme(manifest: dict) -> dict:
    manifest["projects"][0]["url"] = "http://example.invalid/proj.tar.gz"
    return manifest


def _mutate_duplicate_name(manifest: dict) -> dict:
    manifest["projects"].append(dict(manifest["projects"][0]))
    return manifest


def _mutate_traversal_artifact(manifest: dict) -> dict:
    manifest["projects"][0]["artifacts"] = ["../x"]
    return manifest


def _mutate_empty_build(manifest: dict) -> dict:
    manifest["projects"][0]["build"] = []
    return manifest


def _mutate_unsupported_schema_version(manifest: dict) -> dict:
    manifest["schema_version"] = 2
    return manifest


def _mutate_missing_revision(manifest: dict) -> dict:
    del manifest["projects"][0]["revision"]
    return manifest


REJECTION_CASES = [
    pytest.param(_mutate_unpinned_sha, ("proj", "--pin"), id="unpinned-sha-without-flag"),
    pytest.param(_mutate_bad_sha_hex, ("proj", "sha256"), id="sha256-not-64-hex"),
    pytest.param(_mutate_bad_url_scheme, ("proj", "url"), id="url-not-https"),
    pytest.param(_mutate_duplicate_name, ("proj", "duplicate"), id="duplicate-project-name"),
    pytest.param(_mutate_traversal_artifact, ("proj", "artifact"), id="artifact-path-traversal"),
    pytest.param(_mutate_empty_build, ("proj", "build"), id="empty-build-list"),
    pytest.param(_mutate_unsupported_schema_version, ("schema_version",), id="unsupported-schema-version"),
    pytest.param(_mutate_missing_revision, ("proj", "revision"), id="missing-revision-key"),
]


@pytest.mark.parametrize("mutate, expected_substrings", REJECTION_CASES)
def test_load_manifest_rejects_malformed_projects(tmp_path: Path, mutate, expected_substrings) -> None:
    manifest = mutate(_manifest_dict(_valid_project_dict()))
    path = tmp_path / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")

    with pytest.raises(corpus.CorpusError) as error:
        corpus.load_manifest(path)

    message = str(error.value)
    for substring in expected_substrings:
        assert substring in message, f"expected {substring!r} in error message {message!r}"


def test_load_manifest_allows_unpinned_with_explicit_flag(tmp_path: Path) -> None:
    manifest = _manifest_dict(_valid_project_dict(sha256=corpus.UNPINNED))
    path = tmp_path / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")

    projects = corpus.load_manifest(path, allow_unpinned=True)

    assert projects[0].sha256 == corpus.UNPINNED


# ---------------------------------------------------------------------------
# 3. classify: reld/control outcomes -> status, stage, first_line
# ---------------------------------------------------------------------------


CLASSIFY_CASES = [
    pytest.param(
        corpus.Outcome(ok=True, stage=None, output="built fine"),
        None,
        corpus.PASS,
        None,
        id="reld-succeeds",
    ),
    pytest.param(
        corpus.Outcome(ok=False, stage="build", output="ld.reld: error: undefined symbol: foo"),
        corpus.Outcome(ok=True, stage="build", output="ok"),
        corpus.FAIL,
        "build",
        id="build-fails-control-passes",
    ),
    pytest.param(
        corpus.Outcome(ok=False, stage="smoke", output="assertion failed: got 1 want 0"),
        corpus.Outcome(ok=True, stage="smoke", output="ok"),
        corpus.FAIL,
        "smoke",
        id="smoke-fails-control-passes",
    ),
    pytest.param(
        corpus.Outcome(ok=False, stage="build", output="ld.reld: error: relocation truncated"),
        corpus.Outcome(ok=False, stage="build", output="cc: internal compiler error"),
        corpus.HARNESS_ERROR,
        "build",
        id="build-fails-and-so-does-control",
    ),
    pytest.param(
        corpus.Outcome(ok=False, stage="fetch", output="sha256 mismatch"),
        None,
        corpus.HARNESS_ERROR,
        "fetch",
        id="fetch-fails",
    ),
    pytest.param(
        # reld was never actually exercised here (verification failed before it could be); this
        # must never read as a reld pass or a reld failure, only as a harness error.
        corpus.Outcome(ok=False, stage="verify", output="binary was not linked by reld"),
        corpus.Outcome(ok=True, stage="build", output="ok"),
        corpus.HARNESS_ERROR,
        "verify",
        id="verify-fails-despite-a-passing-control",
    ),
]


@pytest.mark.parametrize("reld, control, expected_status, expected_stage", CLASSIFY_CASES)
def test_classify(reld, control, expected_status, expected_stage) -> None:
    status, stage, first_line = corpus.classify(reld, control)

    assert status == expected_status, f"reld={reld} control={control} -> {status}/{stage}/{first_line!r}"
    assert stage == expected_stage

    if expected_status == corpus.PASS:
        assert first_line is None
    elif control is not None and not control.ok:
        assert first_line.startswith("fails without reld too: "), first_line


def test_classify_build_failure_without_control_is_a_harness_bug() -> None:
    reld = corpus.Outcome(ok=False, stage="build", output="ld.reld: error: boom")
    with pytest.raises(ValueError):
        corpus.classify(reld, None)


# ---------------------------------------------------------------------------
# 4. first_failure_line
# ---------------------------------------------------------------------------


FIRST_FAILURE_LINE_CASES = [
    pytest.param(NOISY_BUILD_OUTPUT, None, "ld.reld: error: undefined symbol: foo", id="picks-the-error-line-out-of-noise"),
    pytest.param(
        "checking things...\ncompiling objects\nbuild finished\n\n",
        None,
        "build finished",
        id="falls-back-to-the-last-non-empty-line",
    ),
    pytest.param("", 2, "exit 2", id="empty-output-uses-the-returncode"),
    pytest.param("", None, "no output", id="empty-output-without-a-returncode"),
]


@pytest.mark.parametrize("output, returncode, expected", FIRST_FAILURE_LINE_CASES)
def test_first_failure_line(output, returncode, expected) -> None:
    assert corpus.first_failure_line(output, returncode) == expected


def test_first_failure_line_truncates_a_long_line_without_a_newline() -> None:
    line = "ld.reld: error: " + ("x" * 1000)
    result = corpus.first_failure_line(line)
    assert len(result) <= corpus.FIRST_LINE_LIMIT, len(result)
    assert "\n" not in result


# ---------------------------------------------------------------------------
# 5. pass_rate / build_report are honest about harness errors
# ---------------------------------------------------------------------------


def test_pass_rate_and_build_report_are_honest_about_harness_errors() -> None:
    results = (
        [_result(status=corpus.PASS, name=f"pass-{i}") for i in range(3)]
        + [_result(status=corpus.FAIL, name="broken", stage="build", first_line="boom", control="pass")]
        + [_result(status=corpus.HARNESS_ERROR, name=f"flaky-{i}", stage="fetch") for i in range(2)]
    )

    # A harness error is excluded from the denominator: it neither inflates nor deflates the rate.
    assert corpus.pass_rate(results) == pytest.approx(0.75)
    assert corpus.pass_rate([r for r in results if r.status == corpus.HARNESS_ERROR]) is None

    report = corpus.build_report(
        results,
        reld_commit="abc123",
        reld_version="0.1.0",
        runner={"os": "Linux"},
        generated_at="2026-01-01T00:00:00Z",
    )

    assert report["totals"] == {"projects": 6, "pass": 3, "fail": 1, "harness_error": 2}
    assert report["pass_rate"] == pytest.approx(0.75)


# ---------------------------------------------------------------------------
# 6. Published JSON has an exact, round-trippable shape
# ---------------------------------------------------------------------------


def test_build_report_round_trips_through_json_with_exact_keys() -> None:
    results = [
        _result(status=corpus.PASS, name="a"),
        _result(status=corpus.FAIL, name="b", stage="build", first_line="boom", control="pass"),
        _result(status=corpus.HARNESS_ERROR, name="c", stage="fetch"),
    ]

    report = corpus.build_report(
        results,
        reld_commit="deadbeef",
        reld_version="0.2.0",
        runner={"os": "Linux", "arch": "x86_64"},
        generated_at="2026-01-01T00:00:00Z",
    )

    round_tripped = json.loads(json.dumps(report))

    assert set(round_tripped) == {
        "schema_version",
        "generated_at",
        "reld",
        "runner",
        "totals",
        "pass_rate",
        "projects",
    }
    assert set(round_tripped["reld"]) == {"commit", "version"}
    for project in round_tripped["projects"]:
        assert set(project) == {
            "name",
            "language",
            "stressors",
            "revision",
            "url",
            "sha256",
            "status",
            "stage",
            "first_line",
            "control",
            "seconds",
        }
        assert project["status"] in {corpus.PASS, corpus.FAIL, corpus.HARNESS_ERROR}, project


# ---------------------------------------------------------------------------
# 7. render_summary
# ---------------------------------------------------------------------------


def test_render_summary_reports_the_rate_and_one_row_per_project_even_with_pipes() -> None:
    results = (
        [_result(status=corpus.PASS, name=f"pass-{i}") for i in range(3)]
        + [
            _result(
                status=corpus.FAIL,
                name="broken",
                stage="build",
                first_line="ld.reld: error: a | b",
                control="pass",
            )
        ]
        + [_result(status=corpus.HARNESS_ERROR, name="flaky", stage="fetch")]
    )
    report = corpus.build_report(
        results,
        reld_commit="abc",
        reld_version="0.1.0",
        runner={"os": "Linux"},
        generated_at="2026-01-01T00:00:00Z",
    )

    summary = corpus.render_summary(report)

    assert summary.startswith("### reld link corpus")
    assert "3/4" in summary  # 3 pass out of 4 reld-attributable outcomes; the harness error is excluded.
    assert "harness" in summary.lower()

    table_lines = [line for line in summary.splitlines() if line.startswith("|")]
    # Header + separator + one row per project: an unescaped "|" in a first_line must not
    # spawn or swallow a row.
    assert len(table_lines) == len(results) + 2


# ---------------------------------------------------------------------------
# 8. fetch: checksum-verified download
# ---------------------------------------------------------------------------


def test_fetch_verifies_downloads_and_rejects_bad_ones(tmp_path: Path) -> None:
    tarball = _local_tarball(tmp_path)
    digest = hashlib.sha256(tarball.read_bytes()).hexdigest()
    downloads = tmp_path / "downloads"
    downloads.mkdir()

    good = corpus.fetch(_project(sha256=digest), downloads, opener=lambda url, timeout: tarball.open("rb"))
    assert good.is_file()
    assert hashlib.sha256(good.read_bytes()).hexdigest() == digest

    with pytest.raises(corpus.FetchError) as error:
        corpus.fetch(
            _project(sha256="0" * 64, name="mismatched-proj"),
            downloads,
            opener=lambda url, timeout: tarball.open("rb"),
        )
    message = str(error.value)
    assert "sha256 mismatch" in message
    assert "mismatched-proj" in message
    assert not any(p.name.startswith("mismatched-proj") for p in downloads.iterdir())

    def raising_opener(url, timeout):
        raise urllib.error.URLError("no route to host")

    with pytest.raises(corpus.FetchError):
        corpus.fetch(_project(name="unreachable"), downloads, opener=raising_opener)


# ---------------------------------------------------------------------------
# 9. attempt_env: reld is driven only through --ld-path
# ---------------------------------------------------------------------------


def test_attempt_env_drives_reld_only_through_ld_path_for_both_c_and_rust(tmp_path: Path) -> None:
    base_env = {"PATH": "/usr/bin"}

    c_project = _project(language="c", ldflags=("-static",))
    c_with_reld = corpus.attempt_env(c_project, Path("/abs/reld"), cc="clang", cxx="clang++", base_env=base_env, root=tmp_path)
    c_control = corpus.attempt_env(c_project, None, cc="clang", cxx="clang++", base_env=base_env, root=tmp_path)

    assert "--ld-path=/abs/reld" in c_with_reld["LDFLAGS"]
    assert "-static" in c_with_reld["LDFLAGS"]
    assert "--ld-path" not in c_control["LDFLAGS"]
    assert "-static" in c_control["LDFLAGS"]

    rust_project = _project(language="rust", ldflags=())
    rust_with_reld = corpus.attempt_env(
        rust_project, Path("/abs/reld"), cc="clang", cxx="clang++", base_env=base_env, root=tmp_path
    )
    rust_control = corpus.attempt_env(rust_project, None, cc="clang", cxx="clang++", base_env=base_env, root=tmp_path)

    assert rust_with_reld["RUSTFLAGS"] == "-C linker=clang -C link-arg=--ld-path=/abs/reld"
    assert rust_control["RUSTFLAGS"] == "-C linker=clang"
    # rustc's own -C linker must always be the cc driver, never reld directly (reld is a pure
    # linker, only ever invoked via clang's --ld-path).
    assert "linker=/abs/reld" not in rust_with_reld["RUSTFLAGS"]
    assert "linker=reld" not in rust_with_reld["RUSTFLAGS"]


# ---------------------------------------------------------------------------
# 10. run_project end to end, with fake toolchain and fake runner
# ---------------------------------------------------------------------------


RUN_PROJECT_CASES = [
    pytest.param(
        _runner_build_fails_only_with_reld,
        True,
        corpus.FAIL,
        "build",
        corpus.PASS,
        id="build-fails-control-passes",
    ),
    pytest.param(
        _runner_always_fails,
        True,
        corpus.HARNESS_ERROR,
        "build",
        None,
        id="build-fails-and-so-does-control",
    ),
    pytest.param(
        _runner_always_succeeds,
        True,
        corpus.PASS,
        None,
        "not_run",
        id="build-passes-control-is-not-needed",
    ),
    pytest.param(
        _runner_always_succeeds,
        False,
        corpus.HARNESS_ERROR,
        "verify",
        None,
        id="reld-did-not-actually-link-the-binary",
    ),
]


@pytest.mark.parametrize("runner, linked_by_reld_result, expected_status, expected_stage, expected_control", RUN_PROJECT_CASES)
def test_run_project_classifies_the_outcome_end_to_end(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    runner,
    linked_by_reld_result,
    expected_status,
    expected_stage,
    expected_control,
) -> None:
    monkeypatch.setattr(corpus, "linked_by_reld", lambda path: linked_by_reld_result)
    project, opener = _e2e_setup(tmp_path)

    result = corpus.run_project(
        project,
        Path("/abs/reld"),
        tmp_path / "work",
        tmp_path / "downloads",
        runner=runner,
        opener=opener,
    )

    assert result.status == expected_status, f"stage={result.stage} first_line={result.first_line!r}"
    assert result.stage == expected_stage
    if expected_control is not None:
        assert result.control == expected_control
    if expected_status == corpus.FAIL:
        assert "undefined symbol" in result.first_line


# ---------------------------------------------------------------------------
# 11-12. main()
# ---------------------------------------------------------------------------


def test_main_reports_a_missing_reld_without_running_any_project(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    missing_reld = tmp_path / "nope"

    def fail_if_called(*args, **kwargs):
        raise AssertionError("run_project must not be called when --reld does not exist")

    monkeypatch.setattr(corpus, "run_project", fail_if_called)

    exit_code = corpus.main(["--reld", str(missing_reld), "--manifest", str(corpus.DEFAULT_MANIFEST)])

    assert exit_code == 1
    captured = capsys.readouterr()
    assert str(missing_reld) in captured.err


MAIN_PUBLISH_CASES = [
    pytest.param((corpus.PASS, corpus.FAIL), 0, 0.5, id="mixed-pass-and-fail"),
    pytest.param((corpus.HARNESS_ERROR, corpus.HARNESS_ERROR), 2, None, id="all-harness-error"),
]


@pytest.mark.parametrize("statuses, expected_exit_code, expected_pass_rate", MAIN_PUBLISH_CASES)
def test_main_publishes_the_report_and_signals_overall_result(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    statuses,
    expected_exit_code,
    expected_pass_rate,
) -> None:
    manifest_path = tmp_path / "manifest.json"
    manifest_path.write_text(
        json.dumps(_manifest_dict(_valid_project_dict(name="a"), _valid_project_dict(name="b"))),
        encoding="utf-8",
    )
    reld = tmp_path / "reld"
    reld.write_bytes(b"#!/bin/sh\nexit 0\n")
    reld.chmod(0o755)
    out = tmp_path / "out"
    # When this suite runs inside a GitHub Actions job, main() must not append a fake summary
    # to the real step summary or emit real annotations.
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)

    canned = iter(_canned_result(name, status) for name, status in zip(("a", "b"), statuses))
    monkeypatch.setattr(corpus, "run_project", lambda *args, **kwargs: next(canned))
    monkeypatch.setattr(corpus, "reld_identity", lambda *args, **kwargs: ("abc", "Reld test"))
    monkeypatch.setattr(corpus, "runner_info", lambda *args, **kwargs: {"name": "test"})

    exit_code = corpus.main(
        [
            "--reld",
            str(reld),
            "--manifest",
            str(manifest_path),
            "--publish",
            "--output-dir",
            str(out),
        ]
    )

    assert exit_code == expected_exit_code
    report = json.loads((out / "corpus.json").read_text(encoding="utf-8"))
    if expected_pass_rate is None:
        assert report["pass_rate"] is None
    else:
        assert report["pass_rate"] == pytest.approx(expected_pass_rate)
        assert report["totals"]["fail"] == 1
    assert (out / "summary.md").is_file()


# ---------------------------------------------------------------------------
# 13. The scheduled workflow, not a per-PR one
# ---------------------------------------------------------------------------


def test_corpus_workflow_runs_on_schedule_and_after_benchmark_stats_not_on_every_push() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")

    assert "workflow_dispatch:" in text
    assert "workflow_run:" in text
    assert 'workflows: ["Benchmark Stats"]' in text
    assert "python -m ci.corpus" in text
    assert "--publish" in text
    assert "github.event.repository.default_branch" in text
    assert "benchmark-stats" in text

    assert "pull_request" not in text
    assert "\n  push:" not in text
    assert "continue-on-error" not in text
    assert "--force" not in text
    assert "linker=reld" not in text

    # corpus.json/summary.md are published onto the benchmark-stats branch, which the benchmark
    # workflow also writes to on every scheduled/default-branch run; a force push from this
    # workflow would clobber that shared history, so only the one job that actually publishes
    # may hold contents: write.
    assert text.count("contents: write") == 1


# ---------------------------------------------------------------------------
# 14. Source guard: reld is a pure linker, never a rustc/cc linker driver name
# ---------------------------------------------------------------------------


def test_corpus_module_drives_reld_only_through_ld_path() -> None:
    source = (Path(__file__).parents[1] / "corpus.py").read_text(encoding="utf-8")
    assert "linker=reld" not in source
