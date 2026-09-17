"""The workspace crate set is fixed (reld#152).

Adding a crate is a reviewed decision, not a drive-by one: `AGENTS.md` requires developer
approval in the motivating issue before the work starts. This pins the member list so a new
member cannot arrive without a reviewed change to this file, and pins `publish = false`, since
reld ships as per-platform binaries (reld#148) rather than to crates.io.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

GUIDANCE = (
    "The workspace crate set is fixed. Adding a crate needs developer approval in the "
    "motivating issue first, against the criteria in AGENTS.md; see reld#152. Prefer a module."
)

APPROVED_MEMBERS = {
    "crates/reld",
    "crates/reld-core",
    "crates/reld-diff",
    "crates/reld-layout-schema",
    "crates/reld-reloc",
    "crates/reld-testkit",
    "crates/reld-trace",
}


def workspace_members() -> set[str]:
    manifest = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    return set(manifest["workspace"]["members"])


def test_workspace_members_match_the_approved_set() -> None:
    members = workspace_members()
    added = sorted(members - APPROVED_MEMBERS)
    removed = sorted(APPROVED_MEMBERS - members)
    assert not added, f"New workspace member(s) {added}. {GUIDANCE}"
    assert not removed, f"Workspace member(s) {removed} disappeared; update APPROVED_MEMBERS."


def test_every_crate_is_unpublished() -> None:
    for member in sorted(workspace_members()):
        manifest = tomllib.loads(
            (REPO_ROOT / member / "Cargo.toml").read_text(encoding="utf-8")
        )
        package = manifest["package"]
        assert package.get("publish") is False, (
            f"{member} is publishable. reld ships binaries (reld#148), and publishing to "
            "crates.io is an owner decision tracked in reld#152."
        )
