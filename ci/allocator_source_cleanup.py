"""Remove only validated temporary allocator benchmark source trees."""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SOURCE_PATTERNS = {
    Path("target/allocator-benchmark"): re.compile(r"(?:baseline-source-e2d6be5a|candidate-source-e0868677)\.[A-Za-z0-9]{8}"),
    Path("target/allocator-profiles"): re.compile(r"source-e0868677\.[A-Za-z0-9]{8}"),
}


def cleanup(requested: list[Path], root: Path = REPO_ROOT) -> list[Path]:
    removed: list[Path] = []
    resolved_root = root.resolve()
    allowed: dict[Path, re.Pattern[str]] = {}
    for relative_parent, pattern in SOURCE_PATTERNS.items():
        lexical_parent = resolved_root / relative_parent
        is_junction = getattr(lexical_parent, "is_junction", lambda: False)
        if lexical_parent.is_symlink() or is_junction():
            raise ValueError(f"refusing linked allocator cleanup parent: {lexical_parent}")
        resolved_parent = lexical_parent.resolve()
        if not resolved_parent.is_relative_to(resolved_root):
            raise ValueError(f"refusing allocator cleanup parent outside repository: {resolved_parent}")
        allowed[resolved_parent] = pattern
    for source in requested:
        if not str(source):
            continue
        absolute_source = source if source.is_absolute() else resolved_root / source
        if not absolute_source.exists():
            continue
        if not absolute_source.is_dir() or absolute_source.is_symlink():
            raise ValueError(f"refusing non-directory allocator source cleanup: {absolute_source}")
        resolved_source = absolute_source.resolve()
        name_pattern = allowed.get(resolved_source.parent)
        if name_pattern is None or name_pattern.fullmatch(resolved_source.name) is None:
            raise ValueError(f"refusing unsafe allocator source cleanup: {resolved_source}")
        for path in sorted(resolved_source.rglob("*"), key=lambda item: len(item.parts), reverse=True):
            if path.is_symlink() or path.is_file():
                path.unlink()
            elif path.is_dir():
                path.rmdir()
        resolved_source.rmdir()
        removed.append(resolved_source)
    return removed


if __name__ == "__main__":
    cleanup([Path(argument) for argument in sys.argv[1:] if argument])
