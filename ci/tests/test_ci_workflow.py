from pathlib import Path


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "ci.yml"


def test_ci_caches_linux_reference_linkers():
    text = WORKFLOW.read_text()

    # Linker downloads are cache-gated via actions/cache keyed on pinned versions.
    assert "actions/cache@v4" in text
    assert "reld-linker-cache" in text
    assert "key: linkers-${{ runner.os }}-mold${{ env.MOLD_VERSION }}" in text

    # The heavy download work is delegated to the Python cache-gated script.
    assert "uv run --no-sync python ci/linker_setup.py" in text
    assert "--cache-dir" in text
    assert "--install-debs" in text
    assert "--link-clang" in text


def test_ci_cross_compiles_release_on_linux():
    text = WORKFLOW.read_text()

    # Release cross-compile of the Windows target on a Linux runner, using the
    # mingw-w64 cross toolchain. (soldr's cross path is deferred pending
    # zackees/soldr#2334 / #2335 — see the job comment.)
    assert "cross-release:" in text
    assert "cargo build --release" in text
    assert "x86_64-pc-windows-gnu" in text
    assert "gcc-mingw-w64-x86-64" in text
    assert "CC_x86_64_pc_windows_gnu" in text


def test_ci_measures_target_correct_reld_drivers_in_windows_and_macos_benchmarks():
    text = WORKFLOW.read_text()

    # The PR smokes measure the real platform bridge drivers; there is no permanent pending
    # marker once #17's Windows/macOS benchmark slice is present.
    assert "--reld-pending" not in text
    assert "uv run --no-sync python -m ci.windows_ci benchmark-smoke" in text
    assert "target/debug/ld64.reld" in text
    assert "bash ci/install-driver-shims.sh debug" in text
    assert '--reld "$reld" --workdir "$workdir"' in text


def test_ci_windows_benchmark_gates_generated_and_replayed_large_response_file_paths():
    text = WORKFLOW.read_text()

    # The generated 512-unit path produces the large frozen-object replay fixture. Both tables
    # must prove link.exe, lld, and reld timed successfully, and both run the coverage gate.
    assert "uv run --no-sync python -m ci.windows_ci benchmark-smoke" in text
    assert "benchmark-windows-msvc.md" in text
    assert "benchmark-windows-msvc-replay.md" in text


def test_ci_uses_bash_to_invoke_python_for_every_windows_msvc_script():
    text = WORKFLOW.read_text()

    assert "pwsh" not in text.lower()
    assert "powershell" not in text.lower()
    assert "Tee-Object" not in text
    assert "$env:" not in text
    for command in (
        "verify-msvc-linkers",
        "native-tests",
        "sqlite-bridge",
        "benchmark-smoke",
        "self-host",
    ):
        assert f"shell: bash\n        run: uv run --no-sync python -m ci.windows_ci {command}" in text


def test_ci_macos_benchmark_requires_measured_rows_and_coverage():
    text = WORKFLOW.read_text()

    assert '"macOS benchmark produced no complete measured rows"' in text
    assert "aarch64-apple-darwin" in text
    assert "uv run --no-sync python -m ci.benchmark_coverage" in text


def test_ci_provisions_uv_and_has_no_bare_python_script_invocations():
    text = WORKFLOW.read_text()

    assert text.count("astral-sh/setup-uv@") == 2
    assert text.count("uv sync --extra dev") == 2
    assert "uv run --no-project" not in text
    for line in text.splitlines():
        stripped = line.strip()
        assert not stripped.startswith(("python ", "python3 "))


def test_phase1_summary_only_relaxes_missing_log_validation_after_failure():
    text = WORKFLOW.read_text()

    # The always() summary steps need GitHub's prior-step state explicitly: the summary script
    # cannot infer whether an absent log was skipped because an earlier step failed. Status
    # functions are legal in a step `if`, not in a step `env` expression.
    assert 'PHASE1_UPSTREAM_FAILED: "false"' in text
    assert "if: failure()" in text
    assert "PHASE1_UPSTREAM_FAILED=true" in text
    assert "PHASE1_UPSTREAM_FAILED: ${{ failure() }}" not in text
    assert text.count("--upstream-failed") == 4
