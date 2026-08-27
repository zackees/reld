import json
import subprocess
import sys
from pathlib import Path

from ci.check_dependencies import GUIDANCE, check, inventory


REPO_ROOT = Path(__file__).resolve().parents[2]


def _fixture(tmp_path: Path) -> Path:
    (tmp_path / "crates" / "app").mkdir(parents=True)
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/app"]\n[workspace.dependencies]\napproved = "1"\n',
        encoding="utf-8",
    )
    (tmp_path / "crates" / "app" / "Cargo.toml").write_text(
        '[package]\nname = "app"\nversion = "0.0.0"\n'
        '[dependencies]\napproved.workspace = true\n',
        encoding="utf-8",
    )
    (tmp_path / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "approved"\nversion = "1.0.0"\n'
        'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
        'checksum = "abc"\n',
        encoding="utf-8",
    )
    return tmp_path


def test_current_dependency_graph_matches_approved_baseline() -> None:
    assert check(REPO_ROOT, REPO_ROOT / "ci" / "dependency-baseline.json") == []


def test_ci_runs_dependency_gate() -> None:
    workflow = (REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    assert "python3 ci/check_dependencies.py" in workflow


def test_inventory_covers_every_dependency_kind_and_target(tmp_path: Path) -> None:
    root = _fixture(tmp_path)
    manifest = root / "crates" / "app" / "Cargo.toml"
    manifest.write_text(
        manifest.read_text(encoding="utf-8")
        + '[dev-dependencies]\ndevonly = "1"\n'
        + '[build-dependencies]\nbuildonly = "1"\n'
        + "[target.'cfg(unix)'.dependencies]\ntargetonly = \"1\"\n",
        encoding="utf-8",
    )
    edges = inventory(root)["direct_edges"]
    assert any(edge.endswith(":dev-dependencies:devonly") for edge in edges)
    assert any(edge.endswith(":build-dependencies:buildonly") for edge in edges)
    assert any(edge.endswith(":target.cfg(unix).dependencies:targetonly") for edge in edges)


def test_new_direct_and_transitive_crates_fail_with_required_guidance(tmp_path: Path) -> None:
    root = _fixture(tmp_path)
    baseline = root / "baseline.json"
    baseline.write_text(json.dumps(inventory(root)), encoding="utf-8")
    manifest = root / "crates" / "app" / "Cargo.toml"
    manifest.write_text(
        manifest.read_text(encoding="utf-8") + '[dev-dependencies]\nnew-direct = "1"\n',
        encoding="utf-8",
    )
    with (root / "Cargo.lock").open("a", encoding="utf-8") as lock:
        lock.write(
            '[[package]]\nname = "new-transitive"\nversion = "1.0.0"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
            'checksum = "def"\n'
        )
    additions = check(root, baseline)
    assert any("new-direct" in addition for addition in additions)
    assert any("new-transitive" in addition for addition in additions)

    result = subprocess.run(
        [
            sys.executable,
            str(REPO_ROOT / "ci" / "check_dependencies.py"),
            "--root",
            str(root),
            "--baseline",
            str(baseline),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 1
    assert "New Rust dependency detected:" in result.stderr
    assert GUIDANCE in result.stderr


def test_same_name_version_or_source_substitution_requires_review(tmp_path: Path) -> None:
    root = _fixture(tmp_path)
    baseline = root / "baseline.json"
    baseline.write_text(json.dumps(inventory(root)), encoding="utf-8")
    lock = root / "Cargo.lock"
    original = lock.read_text(encoding="utf-8")
    lock.write_text(original.replace("1.0.0", "1.1.0"), encoding="utf-8")
    assert any("approved|1.1.0" in addition for addition in check(root, baseline))

    lock.write_text(
        original.replace(
            'registry+https://github.com/rust-lang/crates.io-index',
            'git+https://example.invalid/approved?rev=deadbeef',
        ),
        encoding="utf-8",
    )
    assert any("git+https://example.invalid" in addition for addition in check(root, baseline))
