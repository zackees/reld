"""The pinned llvm-ld fetch (reld#96): verification and staging, without the network."""

from __future__ import annotations

import hashlib
import json
import zipfile
from pathlib import Path

import pytest

from ci.fetch_llvm_ld import FetchError
from ci.fetch_llvm_ld import Pin
from ci.fetch_llvm_ld import download
from ci.fetch_llvm_ld import host_os
from ci.fetch_llvm_ld import load_pin
from ci.fetch_llvm_ld import stage

ARCHIVE = "llvm-ld-coff-v0.0.0-x86_64-pc-windows-msvc.zip"


def make_archive(path: Path, *, with_dll: bool = True) -> Path:
    root = "llvm-ld-coff-v0.0.0-x86_64-pc-windows-msvc/"
    with zipfile.ZipFile(path, "w") as bundle:
        if with_dll:
            bundle.writestr(root + "llvm_ld.dll", b"MZ-fake-dll")
        bundle.writestr(root + "llvm_ld.h", b"/* header */")
        bundle.writestr(root + "LICENSE", b"llvm-ld license")
        bundle.writestr(root + "LICENSE-LLVM.txt", b"llvm license")
        bundle.writestr(root + "PROVENANCE.md", b"provenance")
    return path


HOSTS = (
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)


def test_the_committed_lock_pins_every_host_by_checksum() -> None:
    for triple in HOSTS:
        pin = load_pin(triple)
        assert pin is not None, triple
        assert pin.repository == "zackees/llvm-ld"
        assert pin.archive.startswith(f"llvm-ld-coff-{pin.version}-{triple}")
        assert len(pin.sha256) == 64 and all(c in "0123456789abcdef" for c in pin.sha256), triple
        assert pin.url.startswith("https://github.com/zackees/llvm-ld/releases/download/")


def test_host_os_picks_the_library_flavour() -> None:
    assert host_os("aarch64-pc-windows-msvc") == "windows"
    assert host_os("x86_64-unknown-linux-musl") == "linux"
    assert host_os("aarch64-apple-darwin") == "macos"


def test_a_triple_without_a_build_is_no_pin_not_an_error(tmp_path: Path) -> None:
    lock = tmp_path / "lock.json"
    lock.write_text(json.dumps({"repository": "r/r", "version": "v1", "assets": {}}), encoding="utf-8")
    assert load_pin("aarch64-pc-windows-msvc", lock) is None


def test_a_cached_archive_with_the_right_hash_is_reused_without_downloading(tmp_path: Path) -> None:
    archive = make_archive(tmp_path / ARCHIVE)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    pin = Pin("unreachable.invalid/none", "v0", ARCHIVE, digest)
    # The URL is unreachable, so this only passes if the cache hit skips the network entirely.
    assert download(pin, tmp_path) == archive


def test_staging_extracts_the_library_and_renames_llvm_ld_notices(tmp_path: Path) -> None:
    archive = make_archive(tmp_path / ARCHIVE)
    dest = tmp_path / "bin"
    staged = {path.name for path in stage(archive, dest, "llvm_ld.dll")}
    assert staged == {"llvm_ld.dll", "LICENSE-llvm-ld.txt", "LICENSE-LLVM.txt", "PROVENANCE-llvm-ld.md"}
    assert (dest / "llvm_ld.dll").read_bytes() == b"MZ-fake-dll"
    # The header is for build-time consumers; nothing next to reld needs it.
    assert not (dest / "llvm_ld.h").exists()


def test_an_archive_without_the_library_fails_loudly(tmp_path: Path) -> None:
    archive = make_archive(tmp_path / ARCHIVE, with_dll=False)
    with pytest.raises(FetchError, match="does not contain llvm_ld.dll"):
        stage(archive, tmp_path / "bin", "llvm_ld.dll")


def test_a_unix_tarball_stages_the_shared_object_executable(tmp_path: Path) -> None:
    import io
    import tarfile

    archive = tmp_path / "llvm-ld-coff-v0.0.0-x86_64-unknown-linux-gnu.tar.gz"
    with tarfile.open(archive, "w:gz") as bundle:
        for name, data in (("libllvm_ld.so", b"\x7fELF"), ("LICENSE", b"license")):
            info = tarfile.TarInfo(f"llvm-ld-coff-v0.0.0-x86_64-unknown-linux-gnu/{name}")
            info.size = len(data)
            bundle.addfile(info, io.BytesIO(data))
    dest = tmp_path / "bin"
    staged = {path.name for path in stage(archive, dest, "libllvm_ld.so")}
    assert staged == {"libllvm_ld.so", "LICENSE-llvm-ld.txt"}
    assert (dest / "libllvm_ld.so").stat().st_mode & 0o111
