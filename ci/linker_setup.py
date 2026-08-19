"""Cache-gated provisioning of the Linux reference linkers used by CI.

The Phase-1 Linux job pins a set of reference linkers (mold, wild) plus the
`libtinfo5` runtime that clang's LTO plugin needs. Downloading and extracting
those artifacts is the "heavy price" paid on every CI run. This module makes
that work *cache-gated*:

* On a **cache hit** (the artifacts are already present in ``--cache-dir``, e.g.
  restored by ``actions/cache``), it is a no-op — no download, no extraction.
* On a **cache miss** it downloads each pinned artifact, verifies it against the
  pinned SHA-256, and materialises it into ``--cache-dir`` so the surrounding
  ``actions/cache`` step can persist it for the next run.

A SHA mismatch (e.g. a version was bumped without updating the digest here) is a
hard, loud failure rather than a silently-wrong linker.

The download/extraction helpers are deliberately isolated from the pure
"do we already have this?" logic so the latter can be unit-tested without
touching the network.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import tarfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path

CHUNK = 1 << 20


@dataclass(frozen=True)
class Artifact:
    """A pinned, checksum-verified download materialised into the cache dir.

    ``kind`` selects how the downloaded file becomes a usable artifact:

    * ``"tarball"`` — extract and copy the member matching ``member_suffix``
      into ``<cache>/bin/<dest_name>`` and mark it executable.
    * ``"deb"`` — keep the ``.deb`` in ``<cache>/downloads`` and install it with
      ``dpkg`` (the download, not the install, is what we cache).
    """

    name: str
    url: str
    sha256: str
    kind: str
    dest_name: str = ""
    member_suffix: str = ""


def resolve_artifacts(env: dict[str, str]) -> list[Artifact]:
    """Build the concrete artifact list from the pinned version env vars.

    Versions come from the workflow ``env:`` block (single source of truth); the
    SHA-256 digests are pinned here and pair with those exact versions.
    """
    mold_version = env["MOLD_VERSION"]
    wild_version = env["WILD_VERSION"]
    libtinfo5_version = env["LIBTINFO5_VERSION"]
    return [
        Artifact(
            name="mold",
            url=(
                f"https://github.com/rui314/mold/releases/download/v{mold_version}/"
                f"mold-{mold_version}-x86_64-linux.tar.gz"
            ),
            sha256="a3696680d99e692970590a178bc3a33d78d60d1c6dc9db7a11b557b02b751f5d",
            kind="tarball",
            dest_name="mold",
            member_suffix="bin/mold",
        ),
        Artifact(
            name="wild",
            url=(
                f"https://github.com/davidlattimore/wild/releases/download/{wild_version}/"
                f"wild-linker-{wild_version}-x86_64-unknown-linux-gnu.tar.gz"
            ),
            sha256="deb6ee0e5caec798053ec4aafaba042e20a8edf91f08cb4d36268571cc628d3b",
            kind="tarball",
            dest_name="ld.wild",
            member_suffix="wild",
        ),
        Artifact(
            name="libtinfo5",
            url=(
                "https://security.ubuntu.com/ubuntu/pool/universe/n/ncurses/"
                f"libtinfo5_{libtinfo5_version}_amd64.deb"
            ),
            sha256="b9bb64e716a7d9de05b1b33992763142ca81bcae3a7f8ce7e29fa3c6fd32f1e8",
            kind="deb",
        ),
    ]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_satisfied(artifact: Artifact, cache_dir: Path) -> bool:
    """Whether the materialised artifact already exists in the cache (a hit).

    For tarball artifacts the final binary lives in ``<cache>/bin``; for debs the
    cache holds the downloaded ``.deb`` in ``<cache>/downloads``.
    """
    if artifact.kind == "tarball":
        return (cache_dir / "bin" / artifact.dest_name).is_file()
    if artifact.kind == "deb":
        return download_path(artifact, cache_dir).is_file()
    raise ValueError(f"unknown artifact kind {artifact.kind!r}")


def download_path(artifact: Artifact, cache_dir: Path) -> Path:
    return cache_dir / "downloads" / f"{artifact.name}{_url_suffix(artifact.url)}"


def _url_suffix(url: str) -> str:
    name = url.rsplit("/", 1)[-1]
    for suffix in (".tar.gz", ".tgz", ".deb", ".tar.zst"):
        if name.endswith(suffix):
            return suffix
    return ""


def needs_download(artifact: Artifact, cache_dir: Path) -> bool:
    """Whether the raw download must be (re)fetched: absent or wrong digest."""
    cached = download_path(artifact, cache_dir)
    if not cached.is_file():
        return True
    return sha256_file(cached) != artifact.sha256


def _download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(url, headers={"User-Agent": "reld-ci/1.0"})
    with urllib.request.urlopen(request) as response, dest.open("wb") as handle:
        shutil.copyfileobj(response, handle)


def _verify(path: Path, expected: str) -> None:
    actual = sha256_file(path)
    if actual != expected:
        raise SystemExit(
            f"checksum mismatch for {path.name}: expected {expected}, got {actual}"
        )


def _member_matches(name: str, suffix: str) -> bool:
    """True if ``name`` is exactly ``suffix`` or a path component ending in it.

    Handles both a top-level member (``wild`` -> ``wild``) and a nested one
    (``mold-2.41.0-x86_64-linux/bin/mold`` -> ``bin/mold``) without matching an
    unrelated member such as ``mywild``.
    """
    return name == suffix or name.endswith("/" + suffix)


def _extract_member(tarball: Path, member_suffix: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(tarball, "r:gz") as archive:
        match = next(
            (
                member
                for member in archive.getmembers()
                if member.isfile() and _member_matches(member.name, member_suffix)
            ),
            None,
        )
        if match is None:
            raise SystemExit(f"{tarball.name}: no member matching {member_suffix!r}")
        source = archive.extractfile(match)
        if source is None:  # pragma: no cover - defensive
            raise SystemExit(f"{tarball.name}: could not read {match.name}")
        with source, dest.open("wb") as handle:
            shutil.copyfileobj(source, handle)
    dest.chmod(0o755)


def provision(artifact: Artifact, cache_dir: Path) -> str:
    """Ensure ``artifact`` is materialised in ``cache_dir``; return hit|miss.

    Cache hit -> no network, no extraction. Cache miss -> download (if the raw
    file is absent or its digest is stale), verify, and materialise.
    """
    if is_satisfied(artifact, cache_dir):
        return "hit"

    cached = download_path(artifact, cache_dir)
    if needs_download(artifact, cache_dir):
        print(f"downloading {artifact.name} from {artifact.url}")
        _download(artifact.url, cached)
    _verify(cached, artifact.sha256)

    if artifact.kind == "tarball":
        dest = cache_dir / "bin" / artifact.dest_name
        _extract_member(cached, artifact.member_suffix, dest)
    return "miss"


def _install_debs(cache_dir: Path, artifacts: list[Artifact]) -> None:
    debs = [str(download_path(a, cache_dir)) for a in artifacts if a.kind == "deb"]
    if debs:
        subprocess.run(["sudo", "dpkg", "-i", *debs], check=True)


def _link_clang(cache_dir: Path) -> None:
    """Expose the apt-installed clang-22 under stable names in the cache bin dir."""
    bin_dir = cache_dir / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for link_name, target in (("clang", "/usr/bin/clang-22"), ("clang++", "/usr/bin/clang++-22")):
        link = bin_dir / link_name
        if link.is_symlink() or link.exists():
            link.unlink()
        link.symlink_to(target)


def _append_github_path(bin_dir: Path) -> None:
    github_path = os.environ.get("GITHUB_PATH")
    if github_path:
        with Path(github_path).open("a", encoding="utf-8") as handle:
            handle.write(f"{bin_dir}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cache-dir",
        type=Path,
        required=True,
        help="directory persisted by actions/cache (holds bin/ and downloads/)",
    )
    parser.add_argument(
        "--install-debs",
        action="store_true",
        help="dpkg-install cached .deb artifacts (requires sudo)",
    )
    parser.add_argument(
        "--link-clang",
        action="store_true",
        help="symlink apt-installed clang-22/clang++-22 into the cache bin dir",
    )
    parser.add_argument(
        "--no-path",
        action="store_true",
        help="do not append the cache bin dir to GITHUB_PATH",
    )
    args = parser.parse_args()

    cache_dir: Path = args.cache_dir
    (cache_dir / "bin").mkdir(parents=True, exist_ok=True)
    (cache_dir / "downloads").mkdir(parents=True, exist_ok=True)

    artifacts = resolve_artifacts(dict(os.environ))
    for artifact in artifacts:
        outcome = provision(artifact, cache_dir)
        print(f"{artifact.name}: {outcome}")

    if args.install_debs:
        _install_debs(cache_dir, artifacts)
    if args.link_clang:
        _link_clang(cache_dir)
    if not args.no_path:
        _append_github_path(cache_dir / "bin")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
