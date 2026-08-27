"""Reject Rust dependency graph expansion without an approved baseline update."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path
from typing import Any


GUIDANCE = """Adding crates to the reld linker requires explicit developer approval. Agents must not update
the dependency baseline to bypass this check. Use the standard library or an already-approved
dependency, or obtain developer approval and update the baseline in the same reviewed change."""


def _dependency_tables(value: Any, prefix: str = "") -> list[tuple[str, dict[str, Any]]]:
    found: list[tuple[str, dict[str, Any]]] = []
    if not isinstance(value, dict):
        return found
    for key, child in value.items():
        location = f"{prefix}.{key}" if prefix else key
        if key in {"dependencies", "dev-dependencies", "build-dependencies"} and isinstance(
            child, dict
        ):
            found.append((location, child))
        elif isinstance(child, dict):
            found.extend(_dependency_tables(child, location))
    return found


def inventory(root: Path) -> dict[str, list[str]]:
    edges: set[str] = set()
    manifests = [root / "Cargo.toml", *(root / "crates").glob("*/Cargo.toml")]
    for manifest in manifests:
        if not manifest.is_file():
            continue
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        relative = manifest.relative_to(root).as_posix()
        for table, dependencies in _dependency_tables(data):
            for name in dependencies:
                edges.add(f"{relative}:{table}:{name}")

    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    crates = {
        "|".join(
            (
                package["name"],
                package["version"],
                package.get("source", "workspace-or-path"),
                package.get("checksum", "no-checksum"),
            )
        )
        for package in lock.get("package", [])
    }
    return {"direct_edges": sorted(edges), "resolved_crates": sorted(crates)}


def check(root: Path, baseline_path: Path) -> list[str]:
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    current = inventory(root)
    additions = []
    for category in ("direct_edges", "resolved_crates"):
        for item in sorted(set(current[category]) - set(baseline[category])):
            additions.append(f"{category}: {item}")
    return additions


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--baseline", type=Path)
    args = parser.parse_args()
    baseline = args.baseline or args.root / "ci" / "dependency-baseline.json"
    additions = check(args.root, baseline)
    if additions:
        for addition in additions:
            print(f"New Rust dependency detected: {addition}.", file=sys.stderr)
        print(GUIDANCE, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
