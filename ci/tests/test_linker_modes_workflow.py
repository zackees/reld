from pathlib import Path

WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "linker-modes.yml"


def test_workflow_covers_all_native_hosts() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    assert "runner: ubuntu-24.04" in text
    assert "runner: windows-2022" in text
    assert "runner: macos-14" in text
    for platform in ("linux", "windows", "macos"):
        assert f"platform: {platform}" in text


def test_workflow_is_a_thin_uv_invoker() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    assert "astral-sh/setup-uv@" in text
    assert "uv run --no-project python ci/linker_modes.py" in text
    assert text.count("uv run") == 1

    # Compilation, LTO selection, execution, and route assertions belong to Python.
    assert "cargo build" not in text
    assert "-flto" not in text
    assert "fuse-ld" not in text
    assert "reld: engine=" not in text


def test_workflow_uses_compatible_platform_clang_toolchains() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    linux = text.index("platform: linux")
    windows = text.index("platform: windows")
    macos = text.index("platform: macos")
    assert "install_llvm: false" in text[linux:windows]
    assert "install_llvm: true" in text[windows:macos]
    assert "brew install llvm@18" in text


def test_python_pipeline_owns_every_required_mode() -> None:
    pipeline = (Path(__file__).parents[1] / "linker_modes.py").read_text(encoding="utf-8")
    for mode in ("fast", "thin-lto", "full-lto"):
        assert f'"{mode}"' in pipeline
