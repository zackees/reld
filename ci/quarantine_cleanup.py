"""One-shot cleanup helper for a verified Windows executable moved out of the replay path."""

from __future__ import annotations

import argparse
import time
from pathlib import Path


DEFAULT_DELAY_SECONDS = 5.0


def discard_once(path: Path, *, delay_seconds: float = DEFAULT_DELAY_SECONDS) -> None:
    """Wait for transient loader/Defender handles, then make exactly one delete attempt."""
    time.sleep(delay_seconds)
    try:
        path.unlink(missing_ok=True)
    except PermissionError:
        # The Actions workspace is ephemeral. Do not retry or guess which process owns the file.
        pass


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", type=Path)
    args = parser.parse_args()
    discard_once(args.path)


if __name__ == "__main__":
    main()
