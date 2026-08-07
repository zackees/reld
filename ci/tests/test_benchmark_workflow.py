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


def test_benchmark_workflow_gates_expected_linker_coverage():
    text = WORKFLOW.read_text()

    # The per-platform coverage gate must run on every leg (#63): a missing expected linker
    # fails the build rather than shipping a silent n/a.
    assert "ci.benchmark_coverage" in text
    assert "Assert benchmark coverage" in text

    # reld is pending-by-design on Windows/macOS, so those legs pass --reld-pending; Linux (where
    # reld is expected/measured) must not.
    assert "--reld-pending" in text
    assert text.count("reld_pending:") == 2  # windows + macOS matrix entries only
