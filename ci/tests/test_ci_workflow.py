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


def test_ci_cross_compiles_release_on_linux_with_soldr():
    text = WORKFLOW.read_text()

    assert "cross-release:" in text
    assert "zackees/setup-soldr@v0" in text
    assert "soldr cargo build --release" in text
    assert "x86_64-pc-windows-gnu" in text
    assert "gcc-mingw-w64-x86-64" in text
