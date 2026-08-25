"""Free irrelevant preinstalled SDKs on an ephemeral GitHub Linux benchmark runner."""

from __future__ import annotations

import os
import platform
import shutil
from pathlib import Path


REMOVABLE_RUNNER_PATHS = (
    Path("/usr/local/lib/android"),
    Path("/usr/local/.ghcup"),
    Path("/opt/hostedtoolcache/CodeQL"),
)


def is_root() -> bool:
    geteuid = getattr(os, "geteuid", None)
    return geteuid is not None and geteuid() == 0


def reclaim(paths: tuple[Path, ...] = REMOVABLE_RUNNER_PATHS) -> None:
    if (
        platform.system() != "Linux"
        or os.environ.get("GITHUB_ACTIONS") != "true"
        or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
    ):
        raise RuntimeError("runner disk cleanup is restricted to GitHub-hosted ephemeral Linux runners")
    if not is_root():
        raise RuntimeError("runner disk cleanup requires root")

    before = shutil.disk_usage("/").free
    for path in paths:
        if path.is_symlink():
            path.unlink()
        elif path.exists():
            shutil.rmtree(path)
    after = shutil.disk_usage("/").free
    print(f"Freed {(after - before) / (1024**3):.1f} GiB; {(after / (1024**3)):.1f} GiB available")


if __name__ == "__main__":
    reclaim()
