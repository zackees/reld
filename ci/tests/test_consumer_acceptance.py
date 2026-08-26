import json
import subprocess
from pathlib import Path

import pytest

from ci.consumer_acceptance import (
    CMAKE_LINK_RULES,
    CPP_FIXTURE,
    HOSTS,
    PCRE2_COMMIT,
    PCRE2_TEST_TOOLS,
    RUST_CRATE_VERSION,
    AcceptanceError,
    cmake_environment,
    require_logged_outputs,
    require_logged_rust_binary,
    require_exact_output,
    require_tool,
    rust_environment,
    prepare_pcre2_tests,
)


REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "linker-artifacts.yml"


def test_external_consumers_are_immutably_pinned() -> None:
    assert RUST_CRATE_VERSION == "0.13.0"
    assert len(PCRE2_COMMIT) == 40
    assert all(character in "0123456789abcdef" for character in PCRE2_COMMIT)
    assert PCRE2_TEST_TOOLS.is_file()


def test_rust_environment_selects_exact_linker_and_audit_log(tmp_path: Path) -> None:
    linker = tmp_path / "reld-link.exe"
    invocation_log = tmp_path / "rust-invocations.jsonl"
    target_dir = tmp_path / "target"

    env = rust_environment(HOSTS["windows"], linker, target_dir, invocation_log)

    assert env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] == str(linker)
    assert env["CARGO_TARGET_DIR"] == str(target_dir)
    assert env["RELD_INVOCATION_LOG"] == str(invocation_log)
    assert "CARGO_ENCODED_RUSTFLAGS" not in env


def test_linux_rust_environment_uses_clang_to_select_the_exact_linker(tmp_path: Path) -> None:
    target_dir = tmp_path / "target"
    linker = tmp_path / "reld"
    env = rust_environment(HOSTS["linux"], linker, target_dir, tmp_path / "log.jsonl")

    assert env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"] == "clang"
    assert env["CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS"] == f"-Clink-arg=-fuse-ld={linker}"
    assert env["CARGO_ENCODED_RUSTFLAGS"] == f"-Clink-arg=-fuse-ld={linker}"


def test_macos_rust_environment_applies_direct_flavor_to_host_build_scripts(tmp_path: Path) -> None:
    linker = tmp_path / "reld"
    env = rust_environment(
        HOSTS["macos"], linker, tmp_path / "target", tmp_path / "log.jsonl"
    )

    assert env["CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER"] == "clang"
    assert env["CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS"] == f"-Clink-arg=-fuse-ld={linker}"
    assert env["CARGO_ENCODED_RUSTFLAGS"] == f"-Clink-arg=-fuse-ld={linker}"
    assert env["RUSTFLAGS"] == f"-Clink-arg=-fuse-ld={linker}"


def test_tool_lookup_preserves_proxy_executable_name() -> None:
    assert Path(require_tool("python")).name.casefold().startswith("python")


def test_windows_consumer_environment_drops_unix_locale(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LC_ALL", "C.UTF-8")

    env = cmake_environment(Path("reld-link.exe"), host=HOSTS["windows"])

    assert "LC_ALL" not in env


def test_windows_pcre2_suite_uses_committed_binary_safe_helpers(tmp_path: Path) -> None:
    script = tmp_path / "RunGrepTest.bat"
    script.write_text(
        "set printf=cscript //nologo printf.js\n"
        "set trnull=cscript //nologo trnull.js\n",
        encoding="utf-8",
    )

    prepare_pcre2_tests(HOSTS["windows"], tmp_path)

    updated = script.read_text(encoding="utf-8")
    assert "powershell.exe" in updated
    assert str(PCRE2_TEST_TOOLS) in updated


def test_invocation_log_must_match_expected_output_and_route(tmp_path: Path) -> None:
    output = tmp_path / "build" / "consumer.exe"
    log = tmp_path / "invocations.jsonl"
    record = {
        "schema": 1,
        "status": "success",
        "engine": "lld-link",
        "route_kind": "bridge",
        "working_directory": str(output.parent),
        "output": output.name,
    }
    log.write_text(json.dumps(record) + "\n", encoding="utf-8")

    require_logged_outputs(log, HOSTS["windows"], [output], "consumer")

    with pytest.raises(AcceptanceError, match="no successful reld invocation"):
        require_logged_outputs(log, HOSTS["linux"], [output], "consumer")


def test_exact_execution_output_rejects_stderr() -> None:
    passing = subprocess.CompletedProcess([], 0, stdout="ok\n", stderr="")
    require_exact_output(passing, stdout="ok\n", description="fixture")

    noisy = subprocess.CompletedProcess([], 0, stdout="ok\n", stderr="warning\n")
    with pytest.raises(AcceptanceError, match="output mismatch"):
        require_exact_output(noisy, stdout="ok\n", description="fixture")


def test_missing_invocation_log_is_a_hard_failure(tmp_path: Path) -> None:
    with pytest.raises(AcceptanceError, match="did not create"):
        require_logged_outputs(
            tmp_path / "missing.jsonl",
            HOSTS["linux"],
            [tmp_path / "consumer"],
            "consumer",
        )


def test_rust_link_proof_matches_the_hashed_output_cargo_requested(tmp_path: Path) -> None:
    host = HOSTS["windows"]
    target_dir = tmp_path / "target"
    output = target_dir / host.target / "release" / "deps" / "xsv-deadbeef.exe"
    output.parent.mkdir(parents=True)
    output.write_bytes(b"linked")
    log = tmp_path / "rust-invocations.jsonl"
    log.write_text(
        json.dumps(
            {
                "schema": 1,
                "status": "success",
                "engine": "lld-link",
                "route_kind": "bridge",
                "working_directory": str(tmp_path),
                "output": str(output),
            }
        )
        + "\n",
        encoding="utf-8",
    )

    assert require_logged_rust_binary(log, host, target_dir) == output.resolve()


def test_cmake_override_puts_reld_selection_last() -> None:
    rules = CMAKE_LINK_RULES.read_text(encoding="utf-8")

    assert "CMAKE_C_LINK_EXECUTABLE" in rules
    assert "CMAKE_CXX_LINK_EXECUTABLE" in rules
    for line in rules.splitlines():
        if "<CMAKE_" in line and "LINK_LIBRARIES" in line:
            assert line.index("<LINK_LIBRARIES>") < line.index("-fuse-ld=${RELD_LINKER_SELECTOR}")


def test_cpp_fixture_exercises_cross_translation_unit_mangling() -> None:
    header = (CPP_FIXTURE / "include" / "reld_mangled.hpp").read_text(encoding="utf-8")
    implementation = (CPP_FIXTURE / "src" / "mangled.cpp").read_text(encoding="utf-8")
    consumer = (CPP_FIXTURE / "src" / "main.cpp").read_text(encoding="utf-8")

    assert "namespace reld::consumer::abi" in header
    assert "extern template int weighted_sum<int>" in header
    assert "virtual std::string render" in header
    assert "template int weighted_sum<int>" in implementation
    assert 'formatter->render(weighted) != "reld-cxx-42"' in consumer


def test_three_platform_artifact_workflow_runs_consumer_acceptance() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert "runner: ubuntu-24.04" in workflow
    assert "runner: macos-14" in workflow
    assert "runner: windows-2022" in workflow
    assert "cache: false" in workflow
    assert "prebuild-deps: none" in workflow
    assert (
        'soldr cargo build --locked --release --target "${{ matrix.target }}" '
        '--package reld --bin "${{ matrix.binary }}"'
    ) in workflow
    assert workflow.count("binary: reld\n") == 2
    assert "binary: reld-link" in workflow
    assert 'msvc_link="${VCToolsInstallDir}bin/Hostx64/x64/link.exe"' in workflow
    assert "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=$msvc_link" in workflow
    assert "uv run --no-project python -m ci.consumer_acceptance" in workflow
    assert workflow.index("name: Build linker") < workflow.index(
        "name: Link and run pinned Rust, C, and C++ consumers"
    )
    assert "consumer-invocations-${{ matrix.name }}" in workflow


def test_acceptance_compares_logging_off_and_on_artifact_bytes() -> None:
    source = (REPO_ROOT / "ci" / "consumer_acceptance.py").read_text(encoding="utf-8")

    assert "baseline = executable.read_bytes()" in source
    assert "logging-disabled linker output is not self-deterministic" in source
    assert "logging-enabled linker output is not self-deterministic" in source
    assert "RELD_INVOCATION_LOG changed the linked artifact bytes" in source


def test_dependency_policy_requires_explicit_developer_approval() -> None:
    design = (REPO_ROOT / "DESIGN.md").read_text(encoding="utf-8")
    agents = (REPO_ROOT / "AGENTS.md").read_text(encoding="utf-8")

    assert "requires explicit developer approval" in " ".join(design.split())
    assert "requires explicit developer approval" in " ".join(agents.split())
    assert "Agents must not update" in agents
