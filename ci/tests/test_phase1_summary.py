from pathlib import Path

import pytest

from ci.phase1_summary import counts
from ci.phase1_summary import main


def test_counts_rust_and_mimic_results() -> None:
    text = """
test result: ok. 7 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
100 seeds, 0 differential failures
"""
    assert counts(text) == (107, 2)


def test_counts_failed_rust_result() -> None:
    text = """
test result: FAILED. 396 passed; 10 failed; 1 ignored; 0 measured; 0 filtered out
"""
    assert counts(text) == (406, 1)


def test_counts_ansi_colored_results() -> None:
    text = """
test result: \x1b[32mok\x1b[0m. 7 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
test result: \x1b[31mFAILED\x1b[0m. 396 passed; 10 failed; 1 ignored; 0 measured; 0 filtered out
"""
    assert counts(text) == (413, 3)


def test_successful_path_requires_each_expected_log(tmp_path: Path) -> None:
    versions = tmp_path / "versions.txt"
    versions.write_text("lld 18\n")

    with pytest.raises(SystemExit, match="missing expected test log"):
        main(
            [
                "--job",
                "linux-gnu",
                "--log",
                str(tmp_path / "missing.log"),
                "--versions",
                str(versions),
            ]
        )


def test_upstream_failure_allows_missing_downstream_logs(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    versions = tmp_path / "versions.txt"
    versions.write_text("lld 18\n")
    platform_log = tmp_path / "platform-tests.log"
    platform_log.write_text("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n")
    missing_log = tmp_path / "acceptance-tests.log"

    assert (
        main(
            [
                "--job",
                "linux-gnu",
                "--log",
                str(platform_log),
                "--log",
                str(missing_log),
                "--versions",
                str(versions),
                "--minimum-run",
                "100",
                "--exact-log-total",
                f"{missing_log}=100",
                "--upstream-failed",
                "true",
            ]
        )
        == 0
    )
    output = capsys.readouterr().out
    assert "incomplete: an earlier step failed" in output
    assert f"Missing downstream logs: {missing_log}" in output


def test_upstream_failure_allows_missing_versions_file(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    log = tmp_path / "platform-tests.log"
    log.write_text("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n")
    versions = tmp_path / "versions.txt"

    assert (
        main(
            [
                "--job",
                "windows-msvc",
                "--log",
                str(log),
                "--versions",
                str(versions),
                "--upstream-failed",
                "true",
            ]
        )
        == 0
    )
    assert f"unavailable: {versions} was not produced" in capsys.readouterr().out


def test_successful_path_requires_versions_file(tmp_path: Path) -> None:
    log = tmp_path / "platform-tests.log"
    log.write_text("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n")

    with pytest.raises(SystemExit, match="missing expected versions file"):
        main(
            [
                "--job",
                "windows-msvc",
                "--log",
                str(log),
                "--versions",
                str(tmp_path / "versions.txt"),
            ]
        )
