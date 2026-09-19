"""Fetch the pinned prebuilt llvm-ld-coff library (reld#96).

reld links Windows COFF and MinGW targets in-process through llvm-ld-coff (`zackees/llvm-ld`):
LLD's COFF linker, the lld-link/MSVC and MinGW drivers, as a shared library. It only ever links
Windows PE/COFF outputs; the triple it is published for is the *host* it runs on, so the Linux
and macOS builds are Windows cross-linkers.

reld never compiles it: llvm-ld publishes a checksummed prebuilt per host on its GitHub Releases,
`ci/llvm_ld.lock.json` pins one release by version and SHA-256, and this script downloads and
verifies it. Updating reld never rebuilds llvm-ld, and updating llvm-ld is a one-line change to
the lock file.

A host with no pinned build is not an error. reld falls back to spawning
`lld-link` there, so the script says so and exits cleanly with nothing staged.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import tarfile
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = REPO_ROOT / "ci" / "llvm_ld.lock.json"

#: The loadable library per host OS, placed next to reld where `llvm_ld.rs` looks for it.
LIBRARY_NAMES = {"windows": "llvm_ld.dll", "linux": "libllvm_ld.so", "macos": "libllvm_ld.dylib"}


def host_os(triple: str) -> str:
    if "windows" in triple:
        return "windows"
    if "apple" in triple or "darwin" in triple:
        return "macos"
    if "linux" in triple:
        return "linux"
    raise FetchError(f"no llvm-ld-coff host OS known for {triple}")

#: llvm-ld's own notices, renamed so they cannot collide with reld's LICENSE-* files.
NOTICE_RENAMES = {
    "LICENSE": "LICENSE-llvm-ld.txt",
    "LICENSE-LLVM.txt": "LICENSE-LLVM.txt",
    "LICENSE-MIMALLOC.txt": "LICENSE-MIMALLOC.txt",
    "LICENSE-LIBXML2.txt": "LICENSE-LIBXML2.txt",
    "PROVENANCE.md": "PROVENANCE-llvm-ld.md",
}


class FetchError(RuntimeError):
    """A download or verification failure. Never silently skipped."""


@dataclass(frozen=True)
class Pin:
    repository: str
    version: str
    archive: str
    sha256: str

    @property
    def url(self) -> str:
        return f"https://github.com/{self.repository}/releases/download/{self.version}/{self.archive}"


def load_pin(triple: str, lock_path: Path = LOCK_PATH) -> Pin | None:
    """The pinned archive for `triple`, or None when llvm-ld publishes no build for it."""
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    asset = lock["assets"].get(triple)
    if asset is None:
        return None
    return Pin(
        repository=lock["repository"],
        version=lock["version"],
        archive=asset["archive"],
        sha256=asset["sha256"].lower(),
    )


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(pin: Pin, cache_dir: Path) -> Path:
    """Return a verified copy of the pinned archive, downloading it only if it isn't cached."""
    cache_dir.mkdir(parents=True, exist_ok=True)
    archive = cache_dir / pin.archive
    if archive.is_file() and sha256_of(archive) == pin.sha256:
        return archive
    partial = archive.with_suffix(archive.suffix + ".part")
    with urllib.request.urlopen(pin.url, timeout=120) as response, partial.open("wb") as out:  # noqa: S310 - pinned https URL
        shutil.copyfileobj(response, out)
    actual = sha256_of(partial)
    if actual != pin.sha256:
        partial.unlink(missing_ok=True)
        raise FetchError(f"{pin.archive}: SHA-256 {actual} does not match the pinned {pin.sha256}")
    partial.replace(archive)
    return archive


def _members(archive: Path):
    """Yield (file name, readable stream) for each file in a .zip or .tar.gz release archive."""
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as bundle:
            for member in bundle.namelist():
                if not member.endswith("/"):
                    with bundle.open(member) as stream:
                        yield Path(member).name, stream
    else:
        with tarfile.open(archive, "r:gz") as bundle:
            for member in bundle.getmembers():
                if member.isfile():
                    stream = bundle.extractfile(member)
                    if stream is not None:
                        yield Path(member.name).name, stream


def stage(archive: Path, dest: Path, library_name: str) -> list[Path]:
    """Extract the library and llvm-ld's notices into `dest`, flat, next to reld."""
    dest.mkdir(parents=True, exist_ok=True)
    wanted = {library_name: library_name, **NOTICE_RENAMES}
    staged: list[Path] = []
    for name, stream in _members(archive):
        if name in wanted:
            target = dest / wanted[name]
            with target.open("wb") as out:
                shutil.copyfileobj(stream, out)
            if name == library_name:
                target.chmod(0o755)
            staged.append(target)
    if not any(path.name == library_name for path in staged):
        raise FetchError(f"{archive.name} does not contain {library_name}")
    return staged


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--triple", required=True, help="Rust target triple, e.g. x86_64-pc-windows-msvc")
    parser.add_argument("--dest", required=True, type=Path, help="directory to stage the library into (next to reld)")
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=REPO_ROOT / "target" / "llvm-ld-cache",
        help="where downloaded archives are kept, keyed by name; reused while the pin is unchanged",
    )
    args = parser.parse_args(argv)

    pin = load_pin(args.triple)
    if pin is None:
        print(f"llvm-ld-coff publishes no build for {args.triple}; reld will spawn lld-link there")
        return 0
    try:
        library_name = LIBRARY_NAMES[host_os(args.triple)]
        archive = download(pin, args.cache_dir)
        staged = stage(archive, args.dest, library_name)
    except (FetchError, OSError) as error:
        print(f"error: llvm-ld {pin.version} for {args.triple}: {error}", file=sys.stderr)
        return 1
    for path in staged:
        print(path)
    print(f"staged llvm-ld-coff {pin.version} for host {args.triple} ({pin.sha256[:12]}…)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
