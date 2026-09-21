"""Build manifest.json documents for reld's GitHub Release assets and Pages site.

The published Catalog is deliberately a static HTTP document. Consumers resolve an
asset from it without listing releases through the GitHub API, then download the
immutable ``releases/download/<tag>/...`` URL recorded in the selected Asset.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
from pathlib import Path
from typing import Any


SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
TOOL = "reld"
VERSION_RE = re.compile(r"^v(\d+\.\d+\.\d+)$")
ASSET_RE = re.compile(r"^reld-v(?P<version>\d+\.\d+\.\d+)-(?P<triple>.+)\.(?P<extension>tar\.gz|zip)$")
PLATFORMS = {
    "x86_64-unknown-linux-gnu": {"os": "linux", "arch": "x86_64", "libc": "glibc"},
    "aarch64-unknown-linux-gnu": {"os": "linux", "arch": "aarch64", "libc": "glibc"},
    "x86_64-unknown-linux-musl": {"os": "linux", "arch": "x86_64", "libc": "musl"},
    "x86_64-pc-windows-msvc": {"os": "windows", "arch": "x86_64", "abi": "msvc"},
    "aarch64-pc-windows-msvc": {"os": "windows", "arch": "aarch64", "abi": "msvc"},
    "x86_64-apple-darwin": {"os": "darwin", "arch": "x86_64"},
    "aarch64-apple-darwin": {"os": "darwin", "arch": "aarch64"},
}


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _asset(*, filename: str, size: int, sha256: str, url: str) -> dict[str, Any]:
    return {
        "filename": filename,
        "media_type": "application/zip" if filename.endswith(".zip") else "application/gzip",
        "size_bytes": size,
        "sha256": sha256,
        "urls": [url],
        "provides": ["reld", "reld-link"],
    }


def release_document(
    *,
    repository: str,
    tag: str,
    published_at: str,
    assets: list[dict[str, Any]],
    require_all_platforms: bool = True,
) -> dict[str, Any]:
    version_match = VERSION_RE.fullmatch(tag)
    if version_match is None:
        raise ValueError(f"release tag must be vX.Y.Z, got {tag!r}")
    version = version_match.group(1)
    platforms: list[dict[str, Any]] = []
    seen: set[str] = set()
    for asset in assets:
        name = asset["name"]
        match = ASSET_RE.fullmatch(name)
        if match is None or match.group("version") != version:
            continue
        triple = match.group("triple")
        if triple not in PLATFORMS:
            raise ValueError(f"unknown release target triple in {name!r}")
        if triple in seen:
            raise ValueError(f"duplicate release archive for {triple}")
        seen.add(triple)
        digest = asset["sha256"]
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"invalid SHA-256 for {name!r}")
        platforms.append(
            {
                "platform": PLATFORMS[triple],
                "variant": {},
                "asset": _asset(
                    filename=name,
                    size=int(asset["size"]),
                    sha256=digest,
                    url=f"https://github.com/{repository}/releases/download/{tag}/{name}",
                ),
            }
        )
    if not seen:
        raise ValueError(f"release {tag} has no recognized reld archives")
    if require_all_platforms and set(PLATFORMS) != seen:
        missing = ", ".join(sorted(set(PLATFORMS) - seen))
        raise ValueError(f"release {tag} is missing archives for: {missing}")
    return {
        "$schema": SCHEMA,
        "kind": "Release",
        "schema_version": 1,
        "tool": TOOL,
        "version": version,
        "published_at": published_at,
        "urgency": "medium",
        "min_client_version": 1,
        "platforms": platforms,
        "source": {
            "vcs": "git",
            "repo_url": f"https://github.com/{repository}",
            "ref": tag,
            "archive_url": f"https://github.com/{repository}/archive/refs/tags/{tag}.tar.gz",
        },
    }


def _github_assets(release: dict[str, Any]) -> list[dict[str, Any]]:
    result = []
    for asset in release["assets"]:
        digest = asset.get("digest", "")
        if not isinstance(digest, str) or not digest.startswith("sha256:"):
            raise ValueError(f"GitHub did not provide a SHA-256 digest for {asset['name']!r}")
        result.append({"name": asset["name"], "size": asset["size"], "sha256": digest.removeprefix("sha256:")})
    return result


def catalog_document(*, repository: str, releases: list[dict[str, Any]], online_url: str) -> dict[str, Any]:
    stable_releases: list[dict[str, Any]] = []
    for release in releases:
        tag = release.get("tag_name", "")
        if release.get("draft") or release.get("prerelease") or VERSION_RE.fullmatch(tag) is None:
            continue
        published_at = release.get("published_at")
        if not published_at:
            raise ValueError(f"published release {tag} has no published_at timestamp")
        stable_releases.append(release)
    if not stable_releases:
        raise ValueError("no published stable vX.Y.Z releases found")
    latest_release = max(
        stable_releases,
        key=lambda release: tuple(map(int, release["tag_name"].removeprefix("v").split("."))),
    )
    stable = [
        release_document(
            repository=repository,
            tag=release["tag_name"],
            published_at=release["published_at"],
            assets=_github_assets(release),
            require_all_platforms=release is latest_release,
        )
        for release in stable_releases
    ]
    stable.sort(key=lambda release: release["published_at"], reverse=True)
    latest_version = latest_release["tag_name"].removeprefix("v")
    return {
        "$schema": SCHEMA,
        "kind": "Catalog",
        "schema_version": 1,
        "tool": TOOL,
        "online_url": online_url,
        "channels": {"latest-stable": latest_version, "stable": latest_version},
        "releases": stable,
    }


def _write_json(path: Path, document: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def build_release(args: argparse.Namespace) -> None:
    assets = [
        {"name": path.name, "size": path.stat().st_size, "sha256": _sha256(path)}
        for path in sorted(args.assets_dir.iterdir())
        if path.is_file()
    ]
    _write_json(
        args.output,
        release_document(repository=args.repository, tag=args.tag, published_at=args.published_at, assets=assets),
    )


def build_catalog(args: argparse.Namespace) -> None:
    pages = json.loads(args.releases_json.read_text(encoding="utf-8"))
    if not isinstance(pages, list):
        raise ValueError("GitHub releases JSON must be an array")
    # `gh api --paginate --slurp` returns an outer array with one response
    # array per API page. Accept a bare response array too so the command is
    # easy to exercise locally with a single saved API response.
    if pages and all(isinstance(page, list) for page in pages):
        releases = [release for page in pages for release in page]
    else:
        releases = pages
    if not all(isinstance(release, dict) for release in releases):
        raise ValueError("each GitHub release must be an object")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    _write_json(
        args.output_dir / "manifest.json",
        catalog_document(repository=args.repository, releases=releases, online_url=args.online_url),
    )
    shutil.copy2(args.template, args.output_dir / "index.html")
    (args.output_dir / ".nojekyll").touch()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(required=True)
    release = commands.add_parser("release", help="write one release's Release manifest")
    release.add_argument("--repository", required=True)
    release.add_argument("--tag", required=True)
    release.add_argument("--published-at", required=True)
    release.add_argument("--assets-dir", required=True, type=Path)
    release.add_argument("--output", required=True, type=Path)
    release.set_defaults(run=build_release)
    catalog = commands.add_parser("catalog", help="write the Pages Catalog and site")
    catalog.add_argument("--repository", required=True)
    catalog.add_argument("--releases-json", required=True, type=Path)
    catalog.add_argument("--online-url", required=True)
    catalog.add_argument("--template", required=True, type=Path)
    catalog.add_argument("--output-dir", required=True, type=Path)
    catalog.set_defaults(run=build_catalog)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
