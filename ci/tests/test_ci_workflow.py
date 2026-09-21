import json
import re
from pathlib import Path
from types import SimpleNamespace

from ci import release_manifest, windows_ci


WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "ci.yml"
REPO_ROOT = WORKFLOW.parents[2]


def test_release_manifest_uses_immutable_download_urls_and_exact_platforms():
    assets = [
        {
            "name": f"reld-v0.2.0-{triple}.{'zip' if 'windows' in triple else 'tar.gz'}",
            "size": 123,
            "sha256": f"{index:064x}",
        }
        for index, triple in enumerate(release_manifest.PLATFORMS, start=1)
    ]

    document = release_manifest.release_document(
        repository="zackees/reld", tag="v0.2.0", published_at="2026-09-21T00:00:00Z", assets=assets
    )

    assert document["$schema"] == release_manifest.SCHEMA
    assert document["kind"] == "Release"
    assert document["version"] == "0.2.0"
    assert document["source"]["ref"] == "v0.2.0"
    assert {entry["platform"]["os"] for entry in document["platforms"]} == {"linux", "windows", "darwin"}
    for entry in document["platforms"]:
        url = entry["asset"]["urls"][0]
        assert url.startswith("https://github.com/zackees/reld/releases/download/v0.2.0/")
        assert entry["asset"]["sha256"] in {asset["sha256"] for asset in assets}


def test_release_site_workflow_deploys_a_catalog_from_published_releases(tmp_path: Path):
    workflow = (REPO_ROOT / ".github" / "workflows" / "release-site.yml").read_text(encoding="utf-8")
    assert "workflows: [Release]" in workflow
    assert "types: [completed]" in workflow
    assert "github.event.workflow_run.conclusion == 'success'" in workflow
    assert "github.event.workflow_run.event == 'push'" in workflow
    assert "github.event.workflow_run.event == 'workflow_dispatch'" in workflow
    assert "gh api --paginate --slurp" in workflow
    assert "actions/upload-pages-artifact@56afc609e74202658d3ffba0e8f6dda462b719fa" in workflow
    assert "actions/deploy-pages@d6db90164ac5ed86f2b6aed7e0febac5b3c0c03e" in workflow

    assets = [
        {
            "name": f"reld-v0.2.0-{triple}.{'zip' if 'windows' in triple else 'tar.gz'}",
            "size": 123,
            "digest": f"sha256:{index:064x}",
        }
        for index, triple in enumerate(release_manifest.PLATFORMS, start=1)
    ]
    # This is the exact nested shape emitted by `gh api --paginate --slurp`.
    releases = [[{"tag_name": "v0.2.0", "published_at": "2026-09-21T00:00:00Z", "assets": assets}]]
    releases_path = tmp_path / "releases.json"
    releases_path.write_text(json.dumps(releases), encoding="utf-8")
    output_dir = tmp_path / "site"
    release_manifest.build_catalog(
        SimpleNamespace(
            repository="zackees/reld",
            releases_json=releases_path,
            online_url="https://zackees.github.io/reld/manifest.json",
            template=REPO_ROOT / "site" / "index.html",
            output_dir=output_dir,
        )
    )
    catalog = json.loads((output_dir / "manifest.json").read_text(encoding="utf-8"))
    assert catalog["channels"]["latest-stable"] == "0.2.0"
    assert (output_dir / "index.html").is_file()


def test_catalog_allows_an_older_release_to_have_a_smaller_platform_matrix():
    def github_assets(version: str, triples: list[str]) -> list[dict[str, object]]:
        return [
            {
                "name": f"reld-v{version}-{triple}.{'zip' if 'windows' in triple else 'tar.gz'}",
                "size": 123,
                "digest": f"sha256:{index:064x}",
            }
            for index, triple in enumerate(triples, start=1)
        ]

    releases = [
        {
            "tag_name": "v0.2.0",
            "published_at": "2026-09-21T00:00:00Z",
            "assets": github_assets("0.2.0", list(release_manifest.PLATFORMS)),
        },
        {
            "tag_name": "v0.1.0",
            "published_at": "2026-09-17T00:00:00Z",
            "assets": github_assets("0.1.0", ["x86_64-unknown-linux-gnu"]),
        },
    ]

    catalog = release_manifest.catalog_document(
        repository="zackees/reld",
        releases=releases,
        online_url="https://zackees.github.io/reld/manifest.json",
    )

    assert [release["version"] for release in catalog["releases"]] == ["0.2.0", "0.1.0"]
    assert len(catalog["releases"][1]["platforms"]) == 1


def test_catalog_channel_uses_highest_version_when_an_older_version_was_published_later():
    def github_assets(version: str) -> list[dict[str, object]]:
        return [
            {
                "name": f"reld-v{version}-{triple}.{'zip' if 'windows' in triple else 'tar.gz'}",
                "size": 123,
                "digest": f"sha256:{index:064x}",
            }
            for index, triple in enumerate(release_manifest.PLATFORMS, start=1)
        ]

    catalog = release_manifest.catalog_document(
        repository="zackees/reld",
        releases=[
            {"tag_name": "v0.2.0", "published_at": "2026-09-21T00:00:00Z", "assets": github_assets("0.2.0")},
            {"tag_name": "v0.1.0", "published_at": "2026-09-22T00:00:00Z", "assets": github_assets("0.1.0")},
        ],
        online_url="https://zackees.github.io/reld/manifest.json",
    )

    assert catalog["channels"]["latest-stable"] == "0.2.0"
    assert [release["version"] for release in catalog["releases"]] == ["0.1.0", "0.2.0"]


def test_catalog_rejects_a_historical_release_with_no_recognized_archives():
    complete_assets = [
        {
            "name": f"reld-v0.2.0-{triple}.{'zip' if 'windows' in triple else 'tar.gz'}",
            "size": 123,
            "digest": f"sha256:{index:064x}",
        }
        for index, triple in enumerate(release_manifest.PLATFORMS, start=1)
    ]
    releases = [
        {"tag_name": "v0.2.0", "published_at": "2026-09-21T00:00:00Z", "assets": complete_assets},
        {
            "tag_name": "v0.1.0",
            "published_at": "2026-09-17T00:00:00Z",
            "assets": [{"name": "SHA256SUMS", "size": 123, "digest": f"sha256:{1:064x}"}],
        },
    ]

    try:
        release_manifest.catalog_document(
            repository="zackees/reld",
            releases=releases,
            online_url="https://zackees.github.io/reld/manifest.json",
        )
    except ValueError as error:
        assert "no recognized reld archives" in str(error)
    else:
        raise AssertionError("historical stable releases without reld archives must be rejected")


def test_github_asset_with_no_digest_is_rejected():
    release = {"assets": [{"name": "reld-v0.2.0-x86_64-unknown-linux-gnu.tar.gz", "size": 123, "digest": None}]}

    try:
        release_manifest._github_assets(release)
    except ValueError as error:
        assert "did not provide a SHA-256 digest" in str(error)
    else:
        raise AssertionError("missing GitHub asset digest must be rejected")


def test_normal_toolchains_pin_the_rust_1971_msrv():
    rust_toolchain = (REPO_ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
    manifest = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
    workflow_texts = [
        (REPO_ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
        for name in ("ci.yml", "linker-modes.yml", "stress.yml", "benchmark-stats.yml")
    ]

    assert 'rust-version = "1.97.1"' in manifest
    assert 'channel = "1.97.1"' in rust_toolchain
    assert "RUST_VERSION: 1.97.1" in workflow_texts[0]
    assert all("4716b85f2fac3e324e64fa2810f6b5c3905760a5 # 1.97.1" in text for text in workflow_texts)


def test_ci_caches_linux_reference_linkers():
    text = WORKFLOW.read_text()

    # Linker downloads are cache-gated via actions/cache keyed on pinned versions.
    assert "actions/cache@v4" in text
    assert "reld-linker-cache" in text
    assert "key: linkers-${{ runner.os }}-${{ runner.arch }}-mold${{ env.MOLD_VERSION }}" in text

    # The heavy download work is delegated to the Python cache-gated script.
    assert "uv run --no-sync python ci/linker_setup.py" in text
    assert "--cache-dir" in text
    assert "--install-debs" in text
    assert "--link-clang" in text
    assert '--arch "${{ matrix.arch }}"' in text


def test_ci_cross_compiles_release_on_linux():
    text = WORKFLOW.read_text()

    # Release cross-compile of the Windows target on a Linux runner, via the
    # blessed soldr front door (matches cross-ship.yml's proven shape).
    assert "cross-release:" in text
    assert "zackees/setup-soldr@main" in text
    assert "cross-targets: ${{ matrix.target }}" in text
    assert "soldr build --package reld --bin reld --release --locked" in text
    assert "x86_64-pc-windows-gnu" in text

    # The mingw-w64 toolchain is no longer hand-provisioned on the runner --
    # soldr's catalogue owns compiler/linker/SDK/sysroot selection instead.
    assert "gcc-mingw-w64-x86-64" not in text
    assert "CC_x86_64_pc_windows_gnu" not in text
    assert "CXX_x86_64_pc_windows_gnu" not in text
    assert "AR_x86_64_pc_windows_gnu" not in text
    assert "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER" not in text


def test_ci_leaves_benchmarking_to_the_canonical_dispatched_workflow():
    text = WORKFLOW.read_text()

    # benchmark-stats.yml owns the only benchmark matrix. Keeping the retired synthetic smoke
    # here would reintroduce size scenarios and make the strict LTO coverage gate fail PR CI.
    assert "reld-bench" not in text
    assert "benchmark-smoke" not in text
    assert "small (16 units)" not in text
    assert "medium (128 units)" not in text
    assert "large (512 units)" not in text


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
        "self-host",
    ):
        assert f"shell: bash\n        run: uv run --no-sync python -m ci.windows_ci {command}" in text


def test_ci_provisions_uv_and_has_no_bare_python_script_invocations():
    text = WORKFLOW.read_text()

    # phase1-native, phase1-native-run, and python each provision their own uv.
    assert text.count("astral-sh/setup-uv@") == 3
    assert text.count("uv sync --extra dev") == 3
    assert "uv run --no-project" not in text
    for line in text.splitlines():
        stripped = line.strip()
        assert not stripped.startswith(("python ", "python3 "))


def _job_blocks(text: str) -> dict[str, str]:
    """Split ci.yml's `jobs:` section into name -> block-text, keyed by job id."""

    lines = text.splitlines()
    job_header = re.compile(r"^  ([a-z0-9-]+):$")
    starts: list[tuple[str, int]] = []
    for index, line in enumerate(lines):
        match = job_header.match(line)
        if match:
            starts.append((match.group(1), index))

    blocks: dict[str, str] = {}
    for position, (name, start) in enumerate(starts):
        end = starts[position + 1][1] if position + 1 < len(starts) else len(lines)
        blocks[name] = "\n".join(lines[start:end])
    return blocks


def test_phase1_msvc_and_macos_compile_on_linux_and_only_replay_on_target():
    text = WORKFLOW.read_text()
    blocks = _job_blocks(text)

    cross_build = blocks["phase1-cross-build"]
    assert "runs-on: ubuntu-24.04" in cross_build
    assert "zackees/setup-soldr@main" in cross_build
    assert "version: 0.9.18" in cross_build
    assert "soldr cargo nextest archive" in cross_build
    assert "x86_64-pc-windows-msvc" in cross_build
    assert "aarch64-apple-darwin" in cross_build
    assert "x86_64-pc-windows-gnu" not in cross_build

    native_run = blocks["phase1-native-run"]
    assert "needs: phase1-cross-build" in native_run
    assert "name: Phase 1 / ${{ matrix.name }}" in native_run
    assert "actions/download-artifact@v4" in native_run
    assert "cargo nextest run --archive-file" in native_run
    assert "--workspace-remap" in native_run
    assert "ci.windows_ci native-tests" in native_run
    assert "cargo build --workspace" not in native_run
    assert "$CARGO_COMMAND build --workspace" not in native_run
    assert "$CARGO_COMMAND test" not in native_run

    native = blocks["phase1-native"]
    assert "windows-gnu x86_64" in native
    assert "linux-gnu x86_64" in native
    assert "$CARGO_COMMAND build --workspace --all-targets --target" in native
    assert "windows-msvc" not in native
    assert "macos-14" not in native

    assert 'NEXTEST_VERSION: "0.9.140"' in text
    assert f'PHASE1_NATIVE_FILTER: "{windows_ci.PHASE1_NATIVE_FILTER}"' in text


def test_phase1_native_runs_aarch64_acceptance_on_a_native_arm_runner():
    text = WORKFLOW.read_text()
    native = _job_blocks(text)["phase1-native"]

    # The aarch64 leg is a native ubuntu-24.04-arm runner (mirrors release.yml),
    # not an emulated/cross-compiled leg, targeting the arm64 Ubuntu ports archive.
    assert "linux-gnu aarch64" in native
    assert "os: ubuntu-24.04-arm" in native
    assert "target: aarch64-unknown-linux-gnu" in native
    assert "musl_target: aarch64-unknown-linux-musl" in native
    assert "ports.ubuntu.com/ubuntu-ports" in native
    assert "--job linux-gnu-aarch64" in native

    # It runs the wild ELF acceptance suite (reld#194's scope) and the LTO
    # reference fixtures, parameterized on matrix.arch rather than hard-coded
    # to x86_64.
    assert '$CARGO_COMMAND test -p reld --test acceptance 2>&1 | tee acceptance-tests.log' in native
    assert "elf/${{ matrix.arch }}/wrap-lto/clang" in native
    assert "elf/x86_64/wrap-lto/clang" not in native


def test_phase1_x86_64_linux_summary_is_unchanged():
    text = WORKFLOW.read_text()

    # The x86_64 leg's published summary arguments are byte-for-byte unchanged
    # by the aarch64 leg's addition (only the cache key differs, per its own test).
    assert (
        "ci/phase1_summary.py --job linux-gnu\n"
        "          --log platform-tests.log --log acceptance-tests.log --log difftest.log"
    ) in text
    assert "--minimum-run 507" in text
    assert "--exact-log-total difftest.log=100 --exact-log-total external-tests.log=407" in text


def test_phase1_summary_only_relaxes_missing_log_validation_after_failure():
    text = WORKFLOW.read_text()

    # The always() summary steps need GitHub's prior-step state explicitly: the summary script
    # cannot infer whether an absent log was skipped because an earlier step failed. Status
    # functions are legal in a step `if`, not in a step `env` expression.
    assert 'PHASE1_UPSTREAM_FAILED: "false"' in text
    assert "if: failure()" in text
    assert "PHASE1_UPSTREAM_FAILED=true" in text
    assert "PHASE1_UPSTREAM_FAILED: ${{ failure() }}" not in text
    assert text.count("--upstream-failed") == 5
