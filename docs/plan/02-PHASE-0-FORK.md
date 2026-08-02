# Phase 0 — Fork and foundation

Goal: wild's linker, building and testing green under reld's name, with the licensing correct
and the Windows dispatch point opened. **No new linker functionality in this phase.**

Reference tree for all file paths below: `.extern-repos/wild` (shallow clone, wild-linker
v0.9.0, commit `5793935`).

---

## P0-T0 — Linux dev container ⚠️ added by review (R12) — do this first

**Development happens on Windows 10, and every P0–P2 acceptance command is Linux-only.** wild's
test suite shells out to `gcc`, `clang`, `ld.lld`, `ar`, `objcopy`, `bash`, `getconf`, `qemu-*`;
there is exactly **one** `#[cfg(unix)]` gate in 7,092 lines, so on Windows the tests compile and
fail at runtime rather than skipping. wild's own CI never runs the suite on Windows — its
windows job is `cargo build` only — and does not run it on bare `ubuntu-latest` either, but
inside SHA-pinned prebuilt containers.

Vendor wild's `docker/` directory and `test-config-ci.toml`. State explicitly in the README that
P0–P2 acceptance is run in that container, not on the host.

`Acceptance:` `docker build -f docker/ci/ubuntu.Dockerfile .` succeeds and the container runs
`cargo test --workspace`.

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

**Root support files must be vendored too** (R12) — the original table omitted them and P0's
acceptance depends on them: `fakes/` (the wrapper-script dir the `-B` mechanism uses), the root
`ld` shim, `test-config-ci.toml`, `deny.toml` (which `cargo deny check licenses` needs),
`rustfmt.toml`, `taplo.toml`, `cackle.toml`, `docker/`, and the two git submodules
(`external_test_suites/mold`, `wild/tests/bins`) pinned explicitly. Note git symlinks do not
materialize on a default Windows checkout — use wrapper scripts.

**Edition and MSRV must be reconciled in this same commit** (R12): vendored wild is
`edition = "2024"` with `rust-version = "1.94"`; this workspace is `edition = "2021"` with an
unpinned `stable` toolchain. Bump the workspace edition, set `rust-version`, and pin
`rust-toolchain.toml` to a specific ≥1.94 stable.

Do **not** attempt to preserve wild's git history via subtree. A flat vendored copy with a
recorded upstream SHA is easier to diff and easier to rebase by hand.

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
3. `crates/reld-core/src/args.rs:251` `from_executable_name` — add `"reld-link" => Coff`.
4. `crates/reld-core/src/lib.rs:267` — add the `Args::Coff(..)` arm, `bail!` for now.

⚠️ **Do NOT change `PlatformKind::host()` here** (R13). The original plan set `windows => Coff`
in P0. Since every COFF arm `bail!`s until Phase 3, that would make a bare `reld` on the dev
machine — and every Windows CI job — hit the bail for three phases. Defer the `host()` change to
P3.

⚠️ **`-m i386pep` cannot select the COFF platform** (R2). The original D2/P3-T3 text assumed it
could. wild picks the `Args` variant in `Args::new()` (`args.rs:113-149`) from argv[0], a
literal `-flavor` at argv[1], or the host — **before any parsing**. `-m` is a sub-option inside
the ELF parser whose closures take `&mut ElfArgs` and cannot change which variant exists.
Resolving this needs either a pre-scan of argv before `Args::new()`, or a `CoffArgs` that hosts
the whole GNU dialect — unbudgeted work that lands in the windows-gnu phase, not here.

`Acceptance:` `reld -flavor link --version` prints a version, and `reld -flavor link -o x a.o`
exits non-zero with a message containing "not implemented until Phase 3".

## P0-T5b — Delete the Wasm backend (D14)

The fork inherits a **fourth** platform: `wasm.rs` (5766) + `wasm_writer.rs` (482) +
`args/wasm.rs` (305) + `wasm_wasm32.rs` (105) ≈ 6,650 LOC, plus two CI jobs requiring wasmtime,
wabt, wasi-libc and wasm-tools. Remove `PlatformKind::Wasm` and every arm, drop the CI jobs and
the wasm fixtures, and record the removal in `UPSTREAM.md` as an intentional divergence.

Do this **before** P0-T5's edits so the "add an arm" counts are correct at three platforms.

`Acceptance:` `grep -ri wasm crates/ .github/` returns nothing.

## P0-T7 — Multi-call driver binaries

`ld.reld`, `reld-link` and `ld64.reld` are required by four acceptance commands across the plan
and were created by no task (R17). P0-T1 produces one binary named `reld`, and wild's mechanism
is symlinks, which do not survive a Windows checkout.

Emit them as `[[bin]]` shims or as an install step that copies/hardlinks `reld`.

`Acceptance:` `ld.reld --version && reld-link --version && ld64.reld --version`

## P0-T8 — Reconcile the four existing workflows

P0 breaks all of them (R18). `ci.yml:39-49` runs `cargo run --bin reld -- --targets` and asserts
the binary **exits non-zero by design** — a contract P0 deliberately invalidates. `ci.yml:31-34`
runs `cargo fmt --check` and `clippy -D warnings` over ~30k LOC of vendored upstream code.
`stress.yml` and `sanitizers.yml` trigger on `paths: crates/**`, so every vendored-code PR fires
them.

Drop the smoke assertions, adopt wild's `rustfmt.toml`, narrow the path filters to
`crates/reld-testkit/**`, and reconcile runner pinning (`ubuntu-24.04` vs `ubuntu-latest`).

**State the CI profile explicitly** — `RELD_MALFUNCTION` is `debug_assertions`-gated and
silently vanishes in a release job (D17).

`Acceptance:` `.github/workflows/ci.yml` green on the P0 commit.

## P0-T6 — Preserve the placeholder's honesty

The current `crates/reld/src/main.rs` prints `reld is not implemented yet — see DESIGN.md`. The
README benchmark table publishes `n/a` for the reld column. Both are now **wrong in the other
direction** — after P0 there is a real linker for ELF.

Update `README.md`: state that ELF/Linux works and is inherited from wild, and that Windows and
macOS are not yet implemented. Do not populate the benchmark column here — that is P2-T4, after
the numbers are real and CI-generated.

`Acceptance:` manual review; the README must not claim anything CI has not measured.
