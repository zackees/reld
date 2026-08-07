import hashlib
import io
import json
import tarfile
from pathlib import Path

import pytest

from ci.benchmark_assets import (
    CONFIGURATIONS,
    AssetEntry,
    build_manifest,
    entry_from_archive,
    extract_archive,
    fetch_and_extract,
    load_manifest,
    pack_dir,
    resolve_entry,
    sha256_file,
    validate_manifest,
    write_manifest,
)


def _make_corpus(root: Path) -> Path:
    corpus = root / "corpus"
    (corpus / "objs").mkdir(parents=True)
    (corpus / "objs" / "a.o").write_bytes(b"\x7fELF-a")
    (corpus / "objs" / "b.o").write_bytes(b"\x7fELF-b")
    (corpus / "corpus.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "platform": "x86_64-linux",
                "configuration": "quick",
                "cc": "clang",
                "objects": ["objs/a.o", "objs/b.o"],
                "output_name": "app",
            }
        )
    )
    return corpus


def test_sha256_file(tmp_path: Path) -> None:
    blob = tmp_path / "b"
    blob.write_bytes(b"hello")
    assert sha256_file(blob) == hashlib.sha256(b"hello").hexdigest()


def test_pack_extract_roundtrip_targz(tmp_path: Path) -> None:
    corpus = _make_corpus(tmp_path)
    archive = tmp_path / "corpus.tar.gz"
    pack_dir(corpus, archive)
    assert archive.is_file()
    out = tmp_path / "extracted"
    extract_archive(archive, out)
    assert (out / "corpus.json").is_file()
    assert (out / "objs" / "a.o").read_bytes() == b"\x7fELF-a"


def test_pack_rejects_corpus_without_recipe(tmp_path: Path) -> None:
    d = tmp_path / "bad"
    d.mkdir()
    (d / "stray.o").write_bytes(b"x")
    with pytest.raises(SystemExit):
        pack_dir(d, tmp_path / "out.tar.gz")


def test_extract_rejects_path_traversal(tmp_path: Path) -> None:
    evil = tmp_path / "evil.tar.gz"
    with tarfile.open(evil, "w:gz") as tar:
        data = b"pwned"
        info = tarfile.TarInfo("../escape.txt")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    with pytest.raises(SystemExit):
        extract_archive(evil, tmp_path / "dest")


def test_entry_from_archive_computes_digest_and_url(tmp_path: Path) -> None:
    corpus = _make_corpus(tmp_path)
    archive = tmp_path / "c.tar.gz"
    pack_dir(corpus, archive)
    entry = entry_from_archive(
        platform="x86_64-linux",
        configuration="quick",
        archive=archive,
        archive_path="x86_64-linux/quick/c.tar.gz",
        base_url="https://raw.example/base",
    )
    assert entry.sha256 == sha256_file(archive)
    assert entry.bytes == archive.stat().st_size
    assert entry.archive_url == "https://raw.example/base/x86_64-linux/quick/c.tar.gz"


def test_entry_rejects_unknown_configuration(tmp_path: Path) -> None:
    archive = tmp_path / "c.tar.gz"
    pack_dir(_make_corpus(tmp_path), archive)
    with pytest.raises(SystemExit):
        entry_from_archive(
            platform="x", configuration="bogus", archive=archive, archive_path="x/c.tar.gz"
        )


def _entry(platform: str, configuration: str) -> AssetEntry:
    return AssetEntry(
        platform=platform,
        configuration=configuration,
        archive_path=f"{platform}/{configuration}/c.tar.gz",
        sha256="a" * 64,
        bytes=10,
    )


def test_build_manifest_rejects_duplicates() -> None:
    with pytest.raises(SystemExit):
        build_manifest(
            [_entry("linux", "quick"), _entry("linux", "quick")],
            generated_at="t",
            corpus_version="v1",
        )


def test_configurations_cover_quick_and_lto() -> None:
    assert CONFIGURATIONS == ("quick", "thin-lto", "full-lto")


def test_validate_manifest_good_and_bad() -> None:
    good = build_manifest([_entry("linux", "quick")], generated_at="t", corpus_version="v1")
    validate_manifest(good)  # no raise

    bad_ver = dict(good)
    bad_ver["schema_version"] = 999
    with pytest.raises(SystemExit):
        validate_manifest(bad_ver)

    bad_digest = build_manifest([_entry("linux", "thin-lto")], generated_at="t", corpus_version="v1")
    bad_digest["assets"][0]["sha256"] = "short"
    with pytest.raises(SystemExit):
        validate_manifest(bad_digest)


def test_write_load_roundtrip(tmp_path: Path) -> None:
    manifest = build_manifest(
        [_entry("linux", "quick"), _entry("macos", "full-lto")],
        generated_at="t",
        corpus_version="v1",
    )
    path = tmp_path / "manifest.json"
    write_manifest(manifest, path)
    loaded = load_manifest(path)
    assert len(loaded["assets"]) == 2
    # sorted by (platform, configuration)
    assert loaded["assets"][0]["platform"] == "linux"


def test_resolve_entry() -> None:
    manifest = build_manifest([_entry("linux", "quick")], generated_at="t", corpus_version="v1")
    assert resolve_entry(manifest, "linux", "quick") is not None
    assert resolve_entry(manifest, "linux", "thin-lto") is None


def test_fetch_red_then_green(tmp_path: Path) -> None:
    """RED: no published asset. GREEN: pack + manifest, then fetch+verify+extract."""
    base = tmp_path / "benchmark-assets"
    base.mkdir()

    # RED — manifest has no matching asset yet.
    empty = build_manifest([], generated_at="t", corpus_version="v1")
    with pytest.raises(SystemExit):
        fetch_and_extract(empty, "x86_64-linux", "quick", base=str(base), dest=tmp_path / "d0")

    # Publish: pack the corpus into the assets tree, build the manifest.
    corpus = _make_corpus(tmp_path)
    archive_path = "x86_64-linux/quick/corpus.tar.gz"
    archive = base / archive_path
    pack_dir(corpus, archive)
    entry = entry_from_archive(
        platform="x86_64-linux",
        configuration="quick",
        archive=archive,
        archive_path=archive_path,
    )
    manifest = build_manifest([entry], generated_at="t", corpus_version="v1")

    # GREEN — fetch resolves, verifies sha256, extracts, and yields the recipe.
    out = fetch_and_extract(manifest, "x86_64-linux", "quick", base=str(base), dest=tmp_path / "d1")
    assert (out / "corpus.json").is_file()
    assert (out / "objs" / "a.o").read_bytes() == b"\x7fELF-a"


def test_fetch_detects_tampering(tmp_path: Path) -> None:
    base = tmp_path / "assets"
    archive_path = "x86_64-linux/quick/corpus.tar.gz"
    archive = base / archive_path
    pack_dir(_make_corpus(tmp_path), archive)
    entry = entry_from_archive(
        platform="x86_64-linux", configuration="quick", archive=archive, archive_path=archive_path
    )
    manifest = build_manifest([entry], generated_at="t", corpus_version="v1")
    # Corrupt the published archive after the manifest recorded its digest.
    archive.write_bytes(archive.read_bytes() + b"tampered")
    with pytest.raises(SystemExit):
        fetch_and_extract(manifest, "x86_64-linux", "quick", base=str(base), dest=tmp_path / "d")
