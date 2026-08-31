from pathlib import Path


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "clang-link-replay.yml"


def test_clang_replay_is_a_separate_manual_linux_workflow() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")

    assert "workflow_dispatch:" in text
    assert "pull_request:" not in text
    assert "schedule:" not in text
    assert "benchmark-stats.yml" in text
    assert "runs-on: ubuntu-24.04" in text
    assert "windows" not in text.lower()
    assert "macos" not in text.lower()


def test_clang_replay_pins_baseline_toolchain_and_lock() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")

    assert "6ec92be6674d026e74f7524271fbcbce68b50a39" in text
    assert (
        "rust:1.95.0-bookworm@sha256:"
        "6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1"
    ) in text
    assert "actions/checkout@11d5960a326750d5838078e36cf38b85af677262" in text
    assert "astral-sh/setup-uv@20cfd1bf945f4377ade1205e4dbc17946fc9a30d" in text
    assert "python-version: \"3.12.11\"" in text
    assert "zstd=1.5.4+dfsg2-5" in text
    assert "SSL_CERT_FILE: /etc/ssl/certs/ca-certificates.crt" in text
    assert "CORPUS_LOCK: ci/clang-link-corpus.lock.json" in text
    assert "validate-lock --lock \"$CORPUS_LOCK\"" in text


def test_clang_replay_builds_matching_non_git_version_provenance() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")

    assert 'git archive "$BASELINE_SHA" | tar -xf -' in text
    assert "git archive HEAD | tar -xf -" in text
    assert 'test ! -e "$RUNNER_TEMP/reld-baseline/.git"' in text
    assert 'test ! -e "$RUNNER_TEMP/reld-candidate/.git"' in text
    assert text.count("cargo build --locked --release --package reld --bin reld") == 2
    assert 'cmp "$EVIDENCE_DIR/baseline-version.txt"' in text
    assert 'grep -F "non-git-build"' in text


def test_clang_replay_acquires_locked_asset_and_uploads_failure_evidence() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    validate_at = text.index("validate-lock")
    replay_at = text.index("ci.clang_link_replay replay")

    assert validate_at < replay_at
    assert "--archive" not in text
    assert '--baseline "$EVIDENCE_DIR/baseline-reld"' in text
    assert '--candidate "$EVIDENCE_DIR/candidate-reld"' in text
    assert '--report "$EVIDENCE_DIR/replay-report.json"' in text
    assert "if: always()" in text
    assert "target/clang-link-replay/identity-artifacts/" in text
    assert "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" in text


def test_clang_replay_never_builds_llvm_or_times_compilation() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")

    assert "llvm-project" not in text
    assert "ninja" not in text
    assert "cmake" not in text
    assert "benchmark_runner" not in text
    assert "--trials" not in text
