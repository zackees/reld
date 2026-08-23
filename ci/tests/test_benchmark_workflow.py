from pathlib import Path


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "benchmark-stats.yml"
OBSOLETE_ASSET_WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "benchmark-assets.yml"


def test_benchmark_workflow_publishes_one_directory_per_target():
    text = WORKFLOW.read_text()

    assert "matrix:" in text
    assert "ubuntu-24.04" in text
    assert "windows-2022" in text
    assert "macos-14" in text
    assert "x86_64-linux" in text
    assert "x86_64-pc-windows-msvc" in text
    assert "aarch64-apple-darwin" in text
    assert "needs: benchmark" in text
    assert "benchmark-stats/$target" in text
    assert "github.event.repository.default_branch" in text
    assert not OBSOLETE_ASSET_WORKFLOW.exists()


def test_benchmark_workflow_gates_expected_linker_coverage():
    text = WORKFLOW.read_text()

    # The per-platform coverage gate must run on every leg (#63): a missing expected linker
    # fails the build rather than shipping a silent n/a.
    assert "ci.benchmark_coverage" in text
    assert "Assert benchmark coverage" in text

    # Every target supplies its own front door; no permanent pending bridge path may survive.
    assert "--reld-pending" not in text
    assert "reld_pending:" not in text
    assert "reld_driver: target/release/ld.reld" in text
    assert "reld_driver: target/release/reld-link.exe" in text
    assert "reld_driver: target/release/ld64.reld" in text
    assert '--reld "$RELD_DRIVER"' in text
    assert "pwsh" not in text.lower()
    assert "powershell" not in text.lower()
    assert "uv run --no-sync python -m ci.windows_ci install-benchmark-linkers" in text
    assert "uv run --no-sync python -m ci.windows_ci build-benchmark-driver" in text
    assert "uv run --no-sync python -m ci.benchmark_runner" in text
    assert "--manifest ci/e2e/sqlite-bridge/Cargo.toml" in text
    assert "--print link-args" not in text  # Python owns capture/replay, not shell YAML.
    assert "will report n/a" not in text
    assert "cargo install --locked wild-linker ||" not in text
    assert "ilammy/msvc-dev-cmd@" in text


def test_benchmark_workflow_reports_and_guards_generated_artifacts():
    text = WORKFLOW.read_text()

    assert "Report per-target timings and metadata" in text
    assert "--metadata-output benchmark-output/metadata.json" in text
    assert '--metadata-path "benchmark-input/benchmark-log-$target/metadata.json"' in text
    assert "--summary-only" in text
    assert "--print-targets" in text
    assert "--check-readme README.md" in text
    assert "--verify-current-outputs benchmark-stats" in text
    assert "Report benchmark publication outcome" in text
    assert "GITHUB_STEP_SUMMARY" in text
    assert "permissions:\n  contents: read" in text
    assert "    permissions:\n      contents: write" in text


def test_freshness_watchdog_runs_after_failures_only_on_schedule_or_default_branch():
    text = WORKFLOW.read_text()

    assert "freshness:" in text
    assert "needs: [benchmark, aggregate]" in text
    assert "always()" in text
    assert "github.event_name == 'schedule'" in text
    assert "--check-remote-freshness" in text
    assert '--remote-base-url "$RELD_BENCHMARK_RAW_BASE_URL"' in text
    assert '--expected-sha "$GITHUB_SHA"' in text


def test_benchmark_workflow_provisions_uv_and_has_no_bare_python_invocations():
    text = WORKFLOW.read_text()

    assert text.count("astral-sh/setup-uv@") == 3
    assert text.count("uv sync --extra dev") == 3
    assert "uv run --no-project" not in text
    for line in text.splitlines():
        stripped = line.strip()
        assert not stripped.startswith(("python ", "python3 "))
