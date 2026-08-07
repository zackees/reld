from pathlib import Path


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "benchmark-stats.yml"


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
    assert 'benchmark-stats/$target' in text
    assert "github.event.repository.default_branch" in text
