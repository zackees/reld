from pathlib import Path


ROOT = Path(__file__).parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "benchmark-assets.yml"
DOCS = ROOT / "ci" / "BENCHMARK_ASSETS.md"


def test_workflow_has_pr_validate_and_guarded_publish():
    text = WORKFLOW.read_text()

    # End-to-end validation runs on PRs.
    assert "pull_request:" in text
    assert "validate:" in text
    assert "python ci/benchmark_assets.py pack" in text
    assert "python ci/benchmark_assets.py manifest" in text
    assert "python ci/benchmark_assets.py fetch" in text
    assert "reld-bench --replay-corpus" in text

    # Publish only on the default branch, and never on pull_request.
    assert "publish:" in text
    assert "if: github.event_name != 'pull_request'" in text
    assert "github.event.repository.default_branch" in text
    assert "push --force origin benchmark-assets" in text

    # Verbose about populating the branch.
    assert "force-push of generated content" in text
    assert "skipping publish" in text


def test_docs_record_storage_decision():
    text = DOCS.read_text()
    assert "benchmark-assets" in text
    assert "manifest.json" in text
    assert "single source of truth" in text
