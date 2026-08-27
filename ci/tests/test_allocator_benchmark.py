import os
from pathlib import Path

import pytest

from ci.allocator_benchmark import (
    PROFILER_ENV_VARS,
    BenchmarkFailure,
    _assert_hook_free,
    _assert_no_profile_output,
    _run_native,
    _timed_link,
    bootstrap_geometric_improvement_ci,
    bootstrap_improvement_ci,
    clean_benchmark_environment,
    rotated_order,
    should_keep_allocator,
)
from ci.allocator_profile_runner import profile_evidence, profile_mode_environment, require_identical_artifacts
from ci.allocator_source_cleanup import cleanup


def test_profiler_environment_is_removed_and_contamination_is_reported() -> None:
    environment = {"PATH": "/bin", **{name: "1" for name in PROFILER_ENV_VARS}}

    cleaned, contaminated = clean_benchmark_environment(environment)

    assert cleaned == {"PATH": "/bin"}
    assert contaminated == sorted(PROFILER_ENV_VARS)


def test_round_order_is_seeded_randomized_and_balanced() -> None:
    orders = [rotated_order(("system", "mimalloc"), round_index, seed=93) for round_index in range(10)]

    assert orders == [rotated_order(("system", "mimalloc"), index, seed=93) for index in range(10)]
    assert sum(order[0] == "system" for order in orders) == 5
    assert sum(order[0] == "mimalloc" for order in orders) == 5


def test_bootstrap_ci_is_reproducible_and_reports_candidate_improvement() -> None:
    baseline = [10.0, 10.1, 9.9, 10.0, 10.2, 9.8, 10.0, 10.1, 9.9, 10.0]
    candidate = [8.0, 8.1, 7.9, 8.0, 8.2, 7.8, 8.0, 8.1, 7.9, 8.0]

    first = bootstrap_improvement_ci(baseline, candidate, iterations=2_000, seed=93)
    second = bootstrap_improvement_ci(baseline, candidate, iterations=2_000, seed=93)

    assert first == second
    assert first[0] > 0.15
    assert first[1] > first[0]

    with pytest.raises(BenchmarkFailure, match="strictly positive"):
        bootstrap_improvement_ci([0.0] * 10, [1.0] * 10)


def test_benchmark_module_rejects_profiler_contaminated_parent_environment(tmp_path: Path) -> None:
    from ci import allocator_benchmark

    with pytest.raises(BenchmarkFailure, match="profiler environment contamination"):
        allocator_benchmark.require_clean_parent_environment({"MIMALLOC_DHAT": "1", "PATH": os.defpath})


def test_feature_proof_rejects_compiled_pprof(tmp_path: Path) -> None:
    proof = tmp_path / "features.txt"
    proof.write_text('mimalloc-pprof feature "pprof"\n', encoding="utf-8")

    with pytest.raises(BenchmarkFailure, match="profiling feature"):
        _assert_hook_free(tmp_path / "reld", proof)


def test_profile_output_guard_checks_the_filesystem(tmp_path: Path) -> None:
    _assert_no_profile_output(tmp_path)
    (tmp_path / "dhat-heap.json").write_text("{}", encoding="utf-8")

    with pytest.raises(BenchmarkFailure, match="profiler output contaminated"):
        _assert_no_profile_output(tmp_path)


def test_aggregate_bootstrap_preserves_workload_pairing() -> None:
    cells = [
        ([10.0] * 10, [9.0] * 10),
        ([20.0] * 10, [18.0] * 10),
        ([30.0] * 10, [27.0] * 10),
    ]

    low, high = bootstrap_geometric_improvement_ci(cells, iterations=500, seed=93)

    assert low == pytest.approx(0.1)
    assert high == pytest.approx(0.1)


def test_shell_builds_both_modes_from_exact_git_archive_and_scrubbed_env() -> None:
    script = (Path(__file__).parents[1] / "allocator_benchmark.sh").read_text(encoding="utf-8")

    assert 'git -C "${root}" archive --format=tar --output="${source_archive}" "${candidate_revision}"' in script
    assert 'baseline_source="$(mktemp -d "${state}/baseline-source-e2d6be5a.XXXXXXXX")"' in script
    assert 'candidate_source="$(mktemp -d "${state}/candidate-source-e0868677.XXXXXXXX")"' in script
    assert 'trap \'python3 "${root}/ci/allocator_source_cleanup.py" "${baseline_source}" "${candidate_source}"\' EXIT' in script
    assert 'test "$(git -C "${root}" rev-parse "${candidate_revision}^{tree}")" = "${candidate_tree}"' in script
    assert script.count('"${common_env[@]}" CARGO_TARGET_DIR=') == 2
    assert "env -i" in script
    assert "--no-default-features --features fork,plugins,zstd" in script


def test_diagnostic_mode_environments_are_isolated(tmp_path: Path) -> None:
    output = tmp_path / "profile"

    default = profile_mode_environment({"PATH": "/bin"}, "default", output)
    pprof = profile_mode_environment({"PATH": "/bin"}, "pprof", output)
    dhat = profile_mode_environment({"PATH": "/bin"}, "dhat", output)

    assert default == {"PATH": "/bin", "MIMALLOC_PROF": "0", "MIMALLOC_PROF_ACTIVE": "0", "MIMALLOC_DHAT": "0"}
    assert pprof["MIMALLOC_PROF"] == "1"
    assert pprof["MIMALLOC_PROF_DUMP_AT_EXIT"] == str(output)
    assert pprof["MIMALLOC_DHAT"] == "0"
    assert dhat["MIMALLOC_PROF"] == "0"
    assert dhat["RELD_DHAT_OUTPUT"] == str(output)

    with pytest.raises(BenchmarkFailure, match="contamination"):
        profile_mode_environment({"MIMALLOC_PROF": "1"}, "default", output)


def test_allocator_workflow_uses_and_uploads_exact_archives() -> None:
    workflow = (Path(__file__).parents[2] / ".github" / "workflows" / "allocator-benchmark.yml").read_text(encoding="utf-8")

    assert "fetch-depth: 0" in workflow
    assert "RELD_ALLOCATOR_SOURCE_ARCHIVE: /tmp/reld-e0868677.tar" in workflow
    assert "RELD_ALLOCATOR_BASELINE_ARCHIVE: /tmp/reld-e2d6be5a.tar" in workflow
    assert "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" in workflow


def test_link_and_native_timeouts_are_actionable(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from ci import allocator_benchmark

    def time_out(command, **kwargs):
        raise allocator_benchmark.subprocess.TimeoutExpired(command, kwargs["timeout"])

    monkeypatch.setattr(allocator_benchmark.subprocess, "run", time_out)
    with pytest.raises(BenchmarkFailure, match="link exceeded 12s"):
        _timed_link(
            ["clang", "input.o"],
            cwd=tmp_path,
            environment={},
            timing_file=tmp_path / "time.txt",
            timeout_seconds=12,
        )
    with pytest.raises(BenchmarkFailure, match="native oracle exceeded 7s"):
        _run_native(tmp_path / "app", "", "", {}, tmp_path, timeout_seconds=7)


def test_diagnostic_profiles_must_be_nonempty_and_artifacts_identical(tmp_path: Path) -> None:
    profile = tmp_path / "heap.prof"
    with pytest.raises(BenchmarkFailure, match="emitted no profile"):
        profile_evidence(profile, label="no-LTO/pprof")
    profile.write_bytes(b"profile")
    assert profile_evidence(profile, label="no-LTO/pprof")["size_bytes"] == 7

    require_identical_artifacts({"default": "abc", "pprof": "abc", "dhat": "abc"}, configuration="no-LTO")
    with pytest.raises(BenchmarkFailure, match="artifact identity failed"):
        require_identical_artifacts({"default": "abc", "pprof": "def"}, configuration="no-LTO")


def test_allocator_decision_requires_wall_confidence_and_cpu_rss_non_regression() -> None:
    good = {
        "wall_seconds": {"bootstrap_95_ci": [0.01, 0.10], "non_regressing": True},
        "cpu_seconds": {"bootstrap_95_ci": [-0.01, 0.05], "non_regressing": True},
        "peak_rss_kib": {"bootstrap_95_ci": [-0.02, 0.04], "non_regressing": True},
    }
    assert should_keep_allocator([good, good, good], good)

    cpu_regression = {name: dict(value) for name, value in good.items()}
    cpu_regression["cpu_seconds"]["non_regressing"] = False
    assert not should_keep_allocator([good, cpu_regression, good], good)

    inconclusive_wall = {name: dict(value) for name, value in good.items()}
    inconclusive_wall["wall_seconds"]["bootstrap_95_ci"] = [-0.01, 0.10]
    assert not should_keep_allocator([good, good, good], inconclusive_wall)


def test_source_cleanup_removes_only_validated_temporary_trees(tmp_path: Path) -> None:
    parent = tmp_path / "target" / "allocator-benchmark"
    removable = parent / "candidate-source-e0868677.Ab12Cd34"
    removable.mkdir(parents=True)
    (removable / "nested").mkdir()
    (removable / "nested" / "source.rs").write_text("fn main() {}", encoding="utf-8")
    preserved = parent / "candidate-source-e0868677.Zy98Xw76"
    preserved.mkdir()

    assert cleanup([removable], tmp_path) == [removable.resolve()]
    assert not removable.exists()
    assert preserved.is_dir()

    unrelated = tmp_path / "unrelated"
    unrelated.mkdir()
    with pytest.raises(ValueError, match="unsafe allocator source cleanup"):
        cleanup([unrelated], tmp_path)


def test_source_cleanup_rejects_symlinked_parent(tmp_path: Path) -> None:
    external = tmp_path / "external"
    external.mkdir()
    linked_parent = tmp_path / "target" / "allocator-benchmark"
    linked_parent.parent.mkdir()
    try:
        linked_parent.symlink_to(external, target_is_directory=True)
    except OSError as error:
        pytest.skip(f"directory symlinks unavailable: {error}")
    requested = linked_parent / "candidate-source-e0868677.Ab12Cd34"
    requested.mkdir()

    with pytest.raises(ValueError, match="linked allocator cleanup parent"):
        cleanup([requested], tmp_path)
