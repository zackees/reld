"""Frozen linker-object benchmark assets: manifest, packing, and fetching.

This is the coordination layer for the "compile once, link every iteration"
benchmark (see the meta issue). Linker corpora (frozen object files + a
``corpus.json`` recipe describing how to link them) are built once per platform
per configuration, packed into an archive, and published to the
``benchmark-assets`` branch alongside a single ``manifest.json`` index.

``manifest.json`` is the single source of truth for consumers: it lists, per
``(platform, configuration)``, the archive location, its SHA-256, size, and the
toolchain/corpus metadata. The benchmark then:

    resolve -> fetch (verify sha256) -> extract -> replay the link

with **zero compilation in the loop** — the replay (``reld-bench
--replay-corpus``) only re-runs the link step against the frozen objects.

Archive codec is chosen by extension: ``.tar.gz`` uses the stdlib (portable,
used by the hermetic tests); ``.tar.zst`` shells out to the ``zstd`` CLI (used
in publishing, where zstd's fast high-ratio compression shrinks the blobs).

The pure logic (manifest build/validate/resolve, checksum, codec dispatch) is
kept free of network and toolchain calls so it is unit-testable without a
compiler, a runner, or the network.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import tarfile
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any


MANIFEST_SCHEMA_VERSION = 1
CORPUS_JSON = "corpus.json"
CHUNK = 1 << 20

# The configurations coverage matrix (see the LTO benchmark issue).
CONFIGURATIONS = ("quick", "thin-lto", "full-lto")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


# --------------------------------------------------------------------------- #
# Archive codec (dispatched by extension)
# --------------------------------------------------------------------------- #


def _is_zst(path: Path) -> bool:
    return path.name.endswith(".tar.zst")


def _is_targz(path: Path) -> bool:
    return path.name.endswith((".tar.gz", ".tgz"))


def pack_dir(corpus_dir: Path, out_archive: Path) -> None:
    """Pack a corpus directory into ``out_archive`` (codec by extension).

    The archive stores paths relative to ``corpus_dir`` so extraction restores
    the corpus (including ``corpus.json``) directly into the target directory.
    """
    if not (corpus_dir / CORPUS_JSON).is_file():
        raise SystemExit(f"{corpus_dir} has no {CORPUS_JSON}; refusing to pack an invalid corpus")
    out_archive.parent.mkdir(parents=True, exist_ok=True)
    if _is_targz(out_archive):
        with tarfile.open(out_archive, "w:gz") as tar:
            tar.add(corpus_dir, arcname=".")
    elif _is_zst(out_archive):
        # tar (stdlib) has no zstd; stream a plain tar into the zstd CLI.
        tar_path = out_archive.with_suffix("")  # drop .zst -> .tar
        try:
            with tarfile.open(tar_path, "w") as tar:
                tar.add(corpus_dir, arcname=".")
            subprocess.run(
                ["zstd", "-19", "-q", "-f", "-o", str(out_archive), str(tar_path)],
                check=True,
            )
        finally:
            tar_path.unlink(missing_ok=True)
    else:
        raise SystemExit(f"unsupported archive extension: {out_archive.name}")


def extract_archive(archive: Path, dest: Path) -> None:
    """Extract ``archive`` into ``dest`` (codec by extension)."""
    dest.mkdir(parents=True, exist_ok=True)
    if _is_targz(archive):
        with tarfile.open(archive, "r:gz") as tar:
            _safe_extractall(tar, dest)
    elif _is_zst(archive):
        tar_path = dest / archive.with_suffix("").name  # e.g. corpus.tar
        try:
            subprocess.run(
                ["zstd", "-d", "-q", "-f", "-o", str(tar_path), str(archive)],
                check=True,
            )
            with tarfile.open(tar_path, "r:") as tar:
                _safe_extractall(tar, dest)
        finally:
            tar_path.unlink(missing_ok=True)
    else:
        raise SystemExit(f"unsupported archive extension: {archive.name}")


def _safe_extractall(tar: tarfile.TarFile, dest: Path) -> None:
    """Extract, rejecting members that escape ``dest`` (path traversal)."""
    dest = dest.resolve()
    for member in tar.getmembers():
        target = (dest / member.name).resolve()
        if dest != target and dest not in target.parents:
            raise SystemExit(f"unsafe path in archive: {member.name}")
    # `data` filter (Python 3.12+) blocks absolute paths, traversal, and unsafe
    # members; the explicit check above is belt-and-suspenders.
    tar.extractall(dest, filter="data")  # noqa: S202 - members validated + data filter


# --------------------------------------------------------------------------- #
# Manifest
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class AssetEntry:
    platform: str
    configuration: str
    archive_path: str  # path within the benchmark-assets tree (and www site)
    sha256: str
    bytes: int
    corpus_json: str = CORPUS_JSON  # path to the recipe inside the extracted archive
    archive_url: str = ""  # canonical raw URL, filled by the publisher
    toolchain: dict[str, Any] = field(default_factory=dict)

    def key(self) -> tuple[str, str]:
        return (self.platform, self.configuration)


def entry_from_archive(
    *,
    platform: str,
    configuration: str,
    archive: Path,
    archive_path: str,
    base_url: str = "",
    toolchain: dict[str, Any] | None = None,
) -> AssetEntry:
    """Build a manifest entry for an on-disk archive (computes sha256 + size)."""
    if configuration not in CONFIGURATIONS:
        raise SystemExit(
            f"unknown configuration {configuration!r}; expected one of {CONFIGURATIONS}"
        )
    url = f"{base_url.rstrip('/')}/{archive_path.lstrip('/')}" if base_url else ""
    return AssetEntry(
        platform=platform,
        configuration=configuration,
        archive_path=archive_path,
        sha256=sha256_file(archive),
        bytes=archive.stat().st_size,
        archive_url=url,
        toolchain=dict(toolchain or {}),
    )


def build_manifest(entries: list[AssetEntry], *, generated_at: str, corpus_version: str) -> dict[str, Any]:
    """Assemble the manifest dict. Rejects duplicate (platform, configuration)."""
    seen: set[tuple[str, str]] = set()
    for e in entries:
        if e.key() in seen:
            raise SystemExit(f"duplicate manifest entry for {e.key()}")
        seen.add(e.key())
    return {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "generated_at": generated_at,
        "corpus_version": corpus_version,
        "assets": [asdict(e) for e in sorted(entries, key=lambda e: e.key())],
    }


def validate_manifest(manifest: dict[str, Any]) -> None:
    """Raise SystemExit on any structural problem. Used before publishing."""
    if manifest.get("schema_version") != MANIFEST_SCHEMA_VERSION:
        raise SystemExit(f"manifest schema_version must be {MANIFEST_SCHEMA_VERSION}")
    for field_name in ("generated_at", "corpus_version", "assets"):
        if field_name not in manifest:
            raise SystemExit(f"manifest missing required field {field_name!r}")
    seen: set[tuple[str, str]] = set()
    for a in manifest["assets"]:
        for field_name in ("platform", "configuration", "archive_path", "sha256", "bytes"):
            if not a.get(field_name) and a.get(field_name) != 0:
                raise SystemExit(f"asset missing required field {field_name!r}: {a}")
        if a["configuration"] not in CONFIGURATIONS:
            raise SystemExit(f"asset has unknown configuration {a['configuration']!r}")
        if len(a["sha256"]) != 64:
            raise SystemExit(f"asset sha256 not a hex digest: {a['sha256']!r}")
        key = (a["platform"], a["configuration"])
        if key in seen:
            raise SystemExit(f"duplicate asset {key}")
        seen.add(key)


def resolve_entry(manifest: dict[str, Any], platform: str, configuration: str) -> dict[str, Any] | None:
    """Find the asset for (platform, configuration), or None if not published."""
    for a in manifest["assets"]:
        if a["platform"] == platform and a["configuration"] == configuration:
            return a
    return None


def write_manifest(manifest: dict[str, Any], path: Path) -> None:
    validate_manifest(manifest)
    path.write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def load_manifest(path: Path) -> dict[str, Any]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    validate_manifest(manifest)
    return manifest


# --------------------------------------------------------------------------- #
# Fetch: resolve -> download -> verify -> extract
# --------------------------------------------------------------------------- #


def _read_archive_bytes(entry: dict[str, Any], base: str) -> bytes:
    """Read the archive from ``base`` (a local dir path or an http(s) URL)."""
    if base.startswith(("http://", "https://")):
        url = entry.get("archive_url") or f"{base.rstrip('/')}/{entry['archive_path']}"
        with urllib.request.urlopen(url) as response:  # noqa: S310
            return response.read()
    return (Path(base) / entry["archive_path"]).read_bytes()


def fetch_and_extract(
    manifest: dict[str, Any],
    platform: str,
    configuration: str,
    *,
    base: str,
    dest: Path,
) -> Path:
    """Fetch the (platform, configuration) archive from ``base``, verify, extract.

    ``base`` is either a local directory (the benchmark-assets checkout) or the
    canonical raw URL root. Returns the extracted corpus directory. A checksum
    mismatch is a hard failure — never link against a corrupt/wrong corpus.
    """
    entry = resolve_entry(manifest, platform, configuration)
    if entry is None:
        raise SystemExit(
            f"no published asset for platform={platform} configuration={configuration}"
        )
    data = _read_archive_bytes(entry, base)
    actual = hashlib.sha256(data).hexdigest()
    if actual != entry["sha256"]:
        raise SystemExit(
            f"sha256 mismatch for {entry['archive_path']}: "
            f"expected {entry['sha256']}, got {actual}"
        )
    dest.mkdir(parents=True, exist_ok=True)
    archive_name = Path(entry["archive_path"]).name
    tmp_archive = dest / archive_name
    tmp_archive.write_bytes(data)
    try:
        extract_archive(tmp_archive, dest)
    finally:
        tmp_archive.unlink(missing_ok=True)
    corpus = dest / entry.get("corpus_json", CORPUS_JSON)
    if not corpus.is_file():
        raise SystemExit(f"extracted corpus is missing {entry.get('corpus_json', CORPUS_JSON)}")
    return dest


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #


def _cmd_pack(args: argparse.Namespace) -> int:
    pack_dir(args.corpus_dir, args.out)
    print(f"packed {args.corpus_dir} -> {args.out} ({sha256_file(args.out)}, {args.out.stat().st_size} bytes)")
    return 0


def _cmd_manifest(args: argparse.Namespace) -> int:
    entries: list[AssetEntry] = []
    for spec in args.asset:
        # spec form: platform=<p>,configuration=<c>,archive=<file>,archive_path=<rel>
        fields = dict(kv.split("=", 1) for kv in spec.split(","))
        entries.append(
            entry_from_archive(
                platform=fields["platform"],
                configuration=fields["configuration"],
                archive=Path(fields["archive"]),
                archive_path=fields["archive_path"],
                base_url=args.base_url,
            )
        )
    manifest = build_manifest(
        entries, generated_at=args.generated_at, corpus_version=args.corpus_version
    )
    write_manifest(manifest, args.out)
    print(f"wrote {args.out} with {len(entries)} asset(s)")
    return 0


def _cmd_validate(args: argparse.Namespace) -> int:
    load_manifest(args.manifest)
    print(f"{args.manifest}: valid ({MANIFEST_SCHEMA_VERSION})")
    return 0


def _cmd_fetch(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    corpus = fetch_and_extract(
        manifest, args.platform, args.configuration, base=args.base, dest=args.dest
    )
    print(f"fetched + verified corpus -> {corpus}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("pack", help="pack a corpus dir into an archive (.tar.gz or .tar.zst)")
    p.add_argument("--corpus-dir", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.set_defaults(func=_cmd_pack)

    p = sub.add_parser("manifest", help="build manifest.json from packed archives")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--generated-at", required=True)
    p.add_argument("--corpus-version", required=True)
    p.add_argument("--base-url", default="")
    p.add_argument(
        "--asset",
        action="append",
        default=[],
        help="platform=<p>,configuration=<c>,archive=<file>,archive_path=<rel>",
    )
    p.set_defaults(func=_cmd_manifest)

    p = sub.add_parser("validate", help="validate a manifest.json")
    p.add_argument("--manifest", type=Path, required=True)
    p.set_defaults(func=_cmd_validate)

    p = sub.add_parser("fetch", help="fetch+verify+extract a corpus from a manifest")
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--platform", required=True)
    p.add_argument("--configuration", required=True)
    p.add_argument("--base", required=True, help="local dir or http(s) URL root")
    p.add_argument("--dest", type=Path, required=True)
    p.set_defaults(func=_cmd_fetch)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
