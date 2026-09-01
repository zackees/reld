from pathlib import Path

WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "linux-linker-competition.yml"


def workflow_text() -> str:
    return WORKFLOW.read_text(encoding="utf-8")


def test_competition_workflow_is_linux_vm_only_and_scoped_to_linux_perf_prs() -> None:
    text = workflow_text()

    assert "pull_request:" in text
    assert "workflow_dispatch:" in text
    assert "schedule:" not in text
    assert "runs-on: ubuntu-24.04" in text
    assert "container:" not in text
    assert "startsWith(github.head_ref, 'perf/linux/')" in text
    assert "windows" not in text.lower()
    assert "macos" not in text.lower()


def test_competition_workflow_requires_exact_pr_or_manual_baseline_sha() -> None:
    text = workflow_text()

    assert "baseline_sha:" in text
    assert "required: true" in text
    assert "github.event.pull_request.base.sha" in text
    assert "^[0-9a-f]{40}$" in text
    assert 'git rev-parse --verify "$BASELINE_SHA^{commit}"' in text
    assert "git archive \"$BASELINE_SHA\" | tar -xf -" in text
    assert "git archive HEAD | tar -xf -" in text


def test_competition_workflow_pins_actions_rust_and_comparator_provisioning() -> None:
    text = workflow_text()

    assert "actions/checkout@11d5960a326750d5838078e36cf38b85af677262" in text
    assert "astral-sh/setup-uv@20cfd1bf945f4377ade1205e4dbc17946fc9a30d" in text
    assert "dtolnay/rust-toolchain@e081816240890017053eacbb1bdf337761dc5582" in text
    assert "toolchain: 1.95.0" in text
    assert "COMPARATOR_LOCK: ci/linux-linker-comparators.lock.json" in text
    assert "ci.linux_linker_competition validate-lock --lock \"$COMPARATOR_LOCK\"" in text
    provision_at = text.index("ci.linux_linker_competition provision")
    assert "--lock \"$COMPARATOR_LOCK\"" in text[provision_at:]
    assert "--output-dir \"$COMPARATORS_DIR\"" in text
    assert "apt-get install" not in text
    assert "cargo install" not in text
    assert "brew install" not in text


def test_competition_workflow_preflights_a_delegated_cgroup_v2_root() -> None:
    text = workflow_text()

    assert "CGROUP_ROOT:" in text
    assert "/sys/fs/cgroup" in text
    assert "cgroup.controllers" in text
    assert "cgroup.subtree_control" in text
    assert "+memory +cpu" in text
    assert "sudo chown" in text
    assert "--cgroup-root \"$CGROUP_ROOT\"" in text


def test_competition_workflow_builds_non_git_relds_and_replays_exact_contract() -> None:
    text = workflow_text()

    assert text.count("cargo build --locked --release --package reld --bin reld") == 2
    assert 'test ! -e "$RUNNER_TEMP/reld-baseline/.git"' in text
    assert 'test ! -e "$RUNNER_TEMP/reld-candidate/.git"' in text
    assert "ci.linux_linker_competition replay" in text
    assert "--corpus-lock \"$CORPUS_LOCK\"" in text
    assert "--comparator-lock \"$COMPARATOR_LOCK\"" in text
    assert '--baseline "$EVIDENCE_DIR/baseline-reld"' in text
    assert '--candidate "$EVIDENCE_DIR/candidate-reld"' in text
    assert '--comparators-dir "$COMPARATORS_DIR"' in text
    assert '--workdir "$REPLAY_WORKDIR"' in text
    assert '--report "$REPORT"' in text
    assert "--samples 10 --warmups 2" in text
    assert "llvm-project" not in text
    assert "ninja" not in text
    assert "cmake" not in text


def test_competition_workflow_renders_and_uploads_same_run_evidence_without_history_mutation() -> None:
    text = workflow_text()

    assert "ci.linux_linker_competition_render" in text
    assert '--report "$REPORT"' in text
    assert '--output-dir "$RENDER_DIR"' in text
    assert '--summary-output "$SUMMARY_OUTPUT"' in text
    assert 'cat "$SUMMARY_OUTPUT" >> "$GITHUB_STEP_SUMMARY"' in text
    assert "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02" in text
    assert "if: always()" in text
    assert "if: failure()" in text
    assert "raw-samples.jsonl" in text
    assert "target/linux-linker-competition/identity-artifacts/" in text
    assert "target/linux-linker-competition/mismatch-artifacts/" not in text
    assert ".html" in text
    assert ".png" in text
    assert "benchmark-stats" not in text
    assert "git push" not in text
