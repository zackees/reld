from pathlib import Path

import ci.free_runner_disk as free_disk


def test_cleanup_is_restricted_to_explicit_ephemeral_runner_paths(tmp_path: Path, monkeypatch):
    android = tmp_path / "android"
    codeql = tmp_path / "CodeQL"
    preserved = tmp_path / "cargo"
    for path in (android, codeql, preserved):
        path.mkdir()
        (path / "payload").write_bytes(b"x")

    monkeypatch.setattr(free_disk.platform, "system", lambda: "Linux")
    monkeypatch.setattr(free_disk, "is_root", lambda: True)
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("RUNNER_ENVIRONMENT", "github-hosted")

    free_disk.reclaim((android, codeql))

    assert not android.exists()
    assert not codeql.exists()
    assert preserved.is_dir()


def test_cleanup_refuses_non_github_hosts(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(free_disk.platform, "system", lambda: "Linux")
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)

    try:
        free_disk.reclaim((tmp_path,))
    except RuntimeError as error:
        assert "restricted" in str(error)
    else:
        raise AssertionError("cleanup unexpectedly ran outside GitHub Actions")


def test_cleanup_refuses_self_hosted_runners(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(free_disk.platform, "system", lambda: "Linux")
    monkeypatch.setattr(free_disk, "is_root", lambda: True)
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("RUNNER_ENVIRONMENT", "self-hosted")

    try:
        free_disk.reclaim((tmp_path,))
    except RuntimeError as error:
        assert "GitHub-hosted ephemeral" in str(error)
    else:
        raise AssertionError("cleanup unexpectedly ran on a self-hosted runner")
