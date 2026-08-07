from pathlib import Path


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "ci.yml"


def test_ci_caches_linux_reference_linkers():
    text = WORKFLOW.read_text()

    # Linker downloads are cache-gated via actions/cache keyed on pinned versions.
    assert "actions/cache@v4" in text
    assert "reld-linker-cache" in text
    assert "key: linkers-${{ runner.os }}-mold${{ env.MOLD_VERSION }}" in text

    # The heavy download work is delegated to the Python cache-gated script.
    assert "python3 ci/linker_setup.py" in text
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


def test_ci_marks_reld_pending_on_windows_and_macos_benchmarks():
    text = WORKFLOW.read_text()

    # reld has no shim on the Windows/macOS PR benchmark smokes, so it must render an explicit
    # `pending` (with reason) rather than a silent n/a (#63). Both legs pass --reld-pending.
    assert "--reld-pending" in text
    # Both legs assert the pending marker actually appears in the table (windows + macOS).
    assert text.count("reld must render 'pending'") == 2
