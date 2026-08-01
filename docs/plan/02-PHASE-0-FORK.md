# Phase 0 — Fork and foundation

Goal: wild's linker, building and testing green under reld's name, with the licensing correct
and the Windows dispatch point opened. **No new linker functionality in this phase.**

Reference tree for all file paths below: `.extern-repos/wild` (shallow clone, wild-linker
v0.9.0, commit `5793935`).

---

## P0-T1 — Vendor wild into the workspace

Copy wild's crates into `crates/`, preserving structure:

| From | To |
|---|---|
| `libwild/` | `crates/reld-core/` |
| `wild/` (CLI, 40-line `main.rs`) | `crates/reld/` (replacing the current placeholder) |
| `linker-utils/` | `crates/reld-reloc/` |
| `linker-diff/` | `crates/reld-diff/` |
| `linker-layout/` | `crates/reld-layout-schema/` |
| `linker-trace/` | `crates/reld-trace/` |

Keep `crates/reld-testkit/` as-is — it is ours and already useful.

Do **not** attempt to preserve wild's git history via subtree or submodule. A flat vendored copy
with a recorded upstream SHA is easier to diff and easier to rebase by hand.

`Acceptance:` `cargo build --workspace --all-targets`

## P0-T2 — Licensing and attribution

Non-negotiable and must land in the same commit as P0-T1.

- Copy `LICENSE-MIT` and `LICENSE-APACHE` from wild into the repo root alongside the existing
  `LICENSE`.
- Create `NOTICE` stating that reld is a derivative work of wild by David Lattimore and
  contributors, naming the upstream commit SHA and the dual license.
- Update the root `README.md` "What it is" section to state the fork relationship plainly.
  The README currently positions reld as a *successor to* wild; it must now say **fork of**.
- `Cargo.toml`: set `license = "MIT OR Apache-2.0"` on every vendored crate.

`Acceptance:` `cargo deny check licenses`

## P0-T3 — Upstream tracking

Create `UPSTREAM.md` recording: upstream URL, vendored commit SHA, vendor date, and a short list
of every intentional divergence. Add `ci/upstream-diff.sh` that re-clones wild at a given SHA
and diffs `crates/reld-core/src` against `libwild/src`, so the size of the delta is always
visible.

`Acceptance:` `bash ci/upstream-diff.sh --stat` runs and prints a diffstat.

## P0-T4 — Rename crate-internal identifiers

Mechanical. `libwild` → `reld_core`, `linker_utils` → `reld_reloc`, etc. Environment variables
`WILD_*` → `RELD_*` (there are several: `WILD_WRITE_LAYOUT`, `WILD_UNSUPPORTED`,
`WILD_MALFUNCTION`, `WILD_TEST_CROSS`, `WILD_SNAPSHOT`, `WILD_VERIFY_PLATFORM_REQUIREMENTS`).

Keep module names (`elf.rs`, `layout.rs`, `platform.rs`) **unchanged** — renaming them breaks
every file-path reference in this plan and in `ci/upstream-diff.sh`.

`Acceptance:` `cargo test --workspace` — wild's full suite green under the new names.

## P0-T5 — Open the COFF dispatch point

Extend the platform-selection machinery **without** implementing a backend. Every new arm
`bail!`s with a message that names Phase 3.

Edit points, exact:

1. `crates/reld-core/src/args.rs:223` — add `PlatformKind::Coff` to the enum.
2. `crates/reld-core/src/args.rs:238` `from_flavor` — replace the `"link" => bail!(...)` at
   line 242 with `"link" => PlatformKind::Coff`.
3. `crates/reld-core/src/args.rs:251` `from_executable_name` — add `"reld-link" => Coff`. Note
   MinGW invokes a GNU-syntax `ld`, so the COFF-with-GNU-dialect combination is selected by
   emulation (`-m i386pep`), not by binary name (see D2).
4. `crates/reld-core/src/args.rs:230` `PlatformKind::host()` — `windows => Coff`.
5. `crates/reld-core/src/lib.rs:267` — add the `Args::Coff(..)` arm, `bail!` for now.

`Acceptance:` `reld -flavor link --version` prints a version, and `reld -flavor link -o x a.o`
exits non-zero with a message containing "not implemented until Phase 3".

## P0-T6 — Preserve the placeholder's honesty

The current `crates/reld/src/main.rs` prints `reld is not implemented yet — see DESIGN.md`. The
README benchmark table publishes `n/a` for the reld column. Both are now **wrong in the other
direction** — after P0 there is a real linker for ELF.

Update `README.md`: state that ELF/Linux works and is inherited from wild, and that Windows and
macOS are not yet implemented. Do not populate the benchmark column here — that is P2-T4, after
the numbers are real and CI-generated.

`Acceptance:` manual review; the README must not claim anything CI has not measured.
