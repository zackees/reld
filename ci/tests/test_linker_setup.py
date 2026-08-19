import hashlib
import io
import os
import tarfile
from pathlib import Path

import pytest

from ci.linker_setup import (
    Artifact,
    _append_github_path,
    _download,
    _extract_member,
    _member_matches,
    _url_suffix,
    download_path,
    is_satisfied,
    needs_download,
    provision,
    resolve_artifacts,
    sha256_file,
)

ENV = {
    "MOLD_VERSION": "2.41.0",
    "WILD_VERSION": "0.9.0",
    "LIBTINFO5_VERSION": "6.3-2ubuntu0.2",
}


def test_resolve_artifacts_uses_env_versions() -> None:
    by_name = {a.name: a for a in resolve_artifacts(ENV)}
    assert set(by_name) == {"mold", "wild", "libtinfo5"}
    assert "v2.41.0/mold-2.41.0-x86_64-linux.tar.gz" in by_name["mold"].url
    assert "wild/releases/download/0.9.0/" in by_name["wild"].url
    assert "libtinfo5_6.3-2ubuntu0.2_amd64.deb" in by_name["libtinfo5"].url
    assert by_name["mold"].dest_name == "mold"
    assert by_name["wild"].dest_name == "ld.wild"


def test_url_suffix() -> None:
    assert _url_suffix("https://x/y/mold-1.tar.gz") == ".tar.gz"
    assert _url_suffix("https://x/y/libtinfo5_1_amd64.deb") == ".deb"
    assert _url_suffix("https://x/y/thing") == ""


def test_sha256_file(tmp_path: Path) -> None:
    blob = tmp_path / "blob"
    blob.write_bytes(b"hello")
    assert sha256_file(blob) == hashlib.sha256(b"hello").hexdigest()


def test_download_sends_a_user_agent(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    seen: dict[str, str | None] = {}

    def fake_urlopen(request):
        seen["user_agent"] = request.get_header("User-agent")
        return io.BytesIO(b"payload")

    monkeypatch.setattr("ci.linker_setup.urllib.request.urlopen", fake_urlopen)
    destination = tmp_path / "download"
    _download("https://example.invalid/artifact", destination)
    assert seen["user_agent"] == "reld-ci/1.0"
    assert destination.read_bytes() == b"payload"


def _deb_artifact(sha: str) -> Artifact:
    return Artifact(name="libtinfo5", url="https://x/y/libtinfo5_1_amd64.deb", sha256=sha, kind="deb")


def test_needs_download_true_when_missing(tmp_path: Path) -> None:
    art = _deb_artifact("0" * 64)
    assert needs_download(art, tmp_path) is True


def test_needs_download_false_when_present_and_valid(tmp_path: Path) -> None:
    art = _deb_artifact(hashlib.sha256(b"payload").hexdigest())
    cached = download_path(art, tmp_path)
    cached.parent.mkdir(parents=True)
    cached.write_bytes(b"payload")
    assert needs_download(art, tmp_path) is False


def test_needs_download_true_when_sha_mismatch(tmp_path: Path) -> None:
    art = _deb_artifact("0" * 64)
    cached = download_path(art, tmp_path)
    cached.parent.mkdir(parents=True)
    cached.write_bytes(b"payload")
    assert needs_download(art, tmp_path) is True


def test_is_satisfied_tarball(tmp_path: Path) -> None:
    art = Artifact(
        name="mold",
        url="https://x/y/mold-1.tar.gz",
        sha256="0" * 64,
        kind="tarball",
        dest_name="mold",
        member_suffix="/bin/mold",
    )
    assert is_satisfied(art, tmp_path) is False
    binary = tmp_path / "bin" / "mold"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"\x7fELF")
    assert is_satisfied(art, tmp_path) is True


def test_is_satisfied_deb(tmp_path: Path) -> None:
    art = _deb_artifact("0" * 64)
    assert is_satisfied(art, tmp_path) is False
    cached = download_path(art, tmp_path)
    cached.parent.mkdir(parents=True)
    cached.write_bytes(b"deb")
    assert is_satisfied(art, tmp_path) is True


def test_provision_hit_is_a_noop(tmp_path: Path) -> None:
    """A satisfied tarball must not touch the network — provision returns 'hit'."""
    art = Artifact(
        name="mold",
        url="https://invalid.invalid/should-not-be-fetched.tar.gz",
        sha256="0" * 64,
        kind="tarball",
        dest_name="mold",
        member_suffix="/bin/mold",
    )
    binary = tmp_path / "bin" / "mold"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"\x7fELF")
    assert provision(art, tmp_path) == "hit"


def test_unknown_kind_raises(tmp_path: Path) -> None:
    art = Artifact(name="x", url="https://x/y/z", sha256="0" * 64, kind="bogus")
    with pytest.raises(ValueError):
        is_satisfied(art, tmp_path)


def test_member_matches() -> None:
    assert _member_matches("wild", "wild") is True
    assert _member_matches("wild-linker-0.9.0/wild", "wild") is True
    assert _member_matches("mold-2.41.0-x86_64-linux/bin/mold", "bin/mold") is True
    assert _member_matches("something/mywild", "wild") is False
    assert _member_matches("wild-linker/notes.txt", "wild") is False


def _make_tarball(path: Path, members: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))


def test_extract_member_toplevel_and_nested(tmp_path: Path) -> None:
    tarball = tmp_path / "wild.tar.gz"
    _make_tarball(tarball, {"wild-linker-0.9.0/wild": b"WILDBIN", "wild-linker-0.9.0/README": b"x"})
    dest = tmp_path / "bin" / "ld.wild"
    _extract_member(tarball, "wild", dest)
    assert dest.read_bytes() == b"WILDBIN"
    if os.name == "posix":
        assert dest.stat().st_mode & 0o111  # executable bit set on the linker binary


def test_extract_member_missing_raises(tmp_path: Path) -> None:
    tarball = tmp_path / "empty.tar.gz"
    _make_tarball(tarball, {"dir/other": b"x"})
    with pytest.raises(SystemExit):
        _extract_member(tarball, "wild", tmp_path / "out")


def test_append_github_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path_file = tmp_path / "gh_path"
    monkeypatch.setenv("GITHUB_PATH", str(path_file))
    _append_github_path(tmp_path / "bin")
    assert path_file.read_text().strip() == str(tmp_path / "bin")
