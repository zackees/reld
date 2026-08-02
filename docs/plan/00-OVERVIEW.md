# reld implementation plan — overview

**Audience: the implementing agent.** Every decision that requires judgement has already been
made and is recorded here or in `01-DECISIONS.md`. If you hit something this plan does not
cover, stop and ask rather than improvising — an unplanned improvisation in a linker surfaces
as a miscompiled binary three months later, not as a failing test today.

## Reading order

| Doc | Contents |
|---|---|
| `00-OVERVIEW.md` | This file. Phase map, global rules, definition of done. |
| `01-DECISIONS.md` | Locked decisions (D1–D17) with rationale. Do not relitigate. |
| `02-PHASE-0-FORK.md` | Fork wild, licensing, rename, delete Wasm, argv[0] dispatch. |
| `03-PHASE-1-HARNESS.md` | Acceptance-test infrastructure. Built **before** new format code. |
| `04-PHASE-2-LINUX.md` | Prove inherited ELF works under reld's name and CI. |
| `05-PHASE-3-WINMSVC.md` | **PE/COFF core + MSVC ABI.** The largest phase. |
| `06-PHASE-4-MACOS.md` | Mach-O arm64. |
| `07-PHASE-5-WINGNU.md` | **MinGW ABI** — additive on the Phase 3 COFF core. |
| `08-ACCEPTANCE.md` | Per-platform acceptance criteria and the CI matrix. |
| `09-INCREMENTAL.md` | IX-T0 (measure first) and phases I0–I7. |
| `REVIEW-01.md`, `REVIEW-02.md` | Adversarial review findings R1–R44. **Read before starting any phase.** |

## Phase map

**Revised after adversarial review — see `REVIEW-01.md` and `REVIEW-02.md`. The original map is
superseded.**

```
P0 Fork ──► P1 Harness ──► P2 Linux parity ──► IX Incremental (ELF only) ──► P3 win-msvc
   │                                                                            │
   └─ I0 preconditions run concurrently through P0–P2 ─┘                        ▼
                                                                             P4 macOS
                                                                                │
                                                                                ▼
                                                                             P5 win-gnu
                                                                                │
                                                                                ▼
                                                                        I3.. patching etc.
```

Three orderings are load-bearing:

1. **P1 before any new backend.** The differential harness must be proven against a known-good
   linker before it judges our own new code. A harness first exercised on unproven code cannot
   distinguish "the linker is wrong" from "the harness is wrong."
2. **IX before P3/P4/P5** (D15). The incremental thesis gets proven on ELF alone, before two
   more backends consume the schedule. This directly de-risks the failure mode that stopped
   wild. ⚠️ **IX's content is an open item** — see `REVIEW-02.md` R25; the phase must be
   re-derived from measurement, because the originally specified version targets ~5% of link
   time while string merging is ~66%.
3. **win-msvc before win-gnu** (D3, revised). radlink is an in-tree MSVC-target reference;
   MinGW's requirements — auto-import, default-script emulation, `.rsrc`, `.CRT$X*` — are all in
   the region where it gives zero help.

I0's preconditions (content-addressed identity, global section IDs, layout journaling,
`--verify`, no-global-state, `reld log`) are built **concurrently through P0–P2**, not after.
Retrofitting any of them is far more expensive than maintaining them.

## Global rules

1. **Never widen scope inside a task.** One task, one commit, one acceptance command.
2. **Every task below has an `Acceptance:` line that is a literal shell command.** A task is
   done when that command exits 0 on CI, not when the code looks finished.
3. **No *new* `todo!()` / `unimplemented!()`.** If a path is unsupported, it must `bail!()` with
   a message naming the feature and the input file. wild's Mach-O backend is a 40-`todo!()`
   skeleton and is the anti-pattern: it compiles, reports success, and panics at runtime.
   *Inherited* sites (54 in the vendored tree — 40 Mach-O, 5 `platform.rs`, 9 Wasm) are
   inventoried in `UPSTREAM.md` with a phase assigned to each; the Wasm ones vanish with D14.
   A repo-wide grep gate enforces the allowlist.
4. **Determinism is mandatory from day one.** Every parallel step is followed by a deterministic
   ordering step, keyed on `(archive index, object index, symbol index)`. Retrofitting
   determinism is far harder than maintaining it. (This is radlink's `lnk_obj_is_before`
   discipline; adopt it globally.)
5. **Correctness beats cleverness.** Any uncertainty in an optimized path falls back to the
   slow path. A silent wrong answer is worse than a slow link.
6. **Upstream hygiene.** reld is a fork of wild. Keep `UPSTREAM.md` current and keep the delta
   reviewable for as long as rebasing remains cheap.

## Definition of done, per phase

| Phase | Done when |
|---|---|
| P0 | `cargo test --workspace` green **in the Linux dev container** (P0-T0); Wasm deleted; `ld.reld`/`reld-link`/`ld64.reld` all print a version; existing workflows reconciled. |
| P1 | Harness runs in CI; differential oracle validated against `ld.bfd` **with `--coverage` published**; mutation-injection proves the oracle catches deliberate bugs. The three non-ELF jobs exist and are green with an expected-minimum trial count that ratchets up in P3/P4/P5. |
| P2 | reld self-hosts on Linux; corpus pass-rate published; benchmark `reld` column populated. |
| IX | Per-phase timing split published (IX-T0); daemon scope derived from it and recorded as D16a. |
| P3 | `cargo build --target x86_64-pc-windows-msvc` linked by reld produces a running binary; COFF oracle coverage at floor with a COFF malfunction site. |
| P4 | Rust hello-world **executes** on Apple Silicon (AMFI, not Gatekeeper); C++ unwinds through `__unwind_info`. |
| P5 | `cargo build --target x86_64-pc-windows-gnu` produces a running binary; gdb breakpoint hits. |
| I1+ | See `09-INCREMENTAL.md` — every incremental phase is gated on a correctness oracle, not a speed number. |

## What we inherit vs. what we build

Measured from the wild tree at `.extern-repos/wild`:

| Component | Status on fork |
|---|---|
| ELF backend (`libwild/src/elf.rs` + `elf_writer.rs`, ~12.5k LOC) | **Production.** 5 architectures. Inherited working. |
| `Platform` / `Arch` traits (`libwild/src/platform.rs`, 1573 LOC) | **Real and load-bearing**, but a 163-member interface extracted from ELF. Expect friction. |
| GNU ld argument parser (`libwild/src/args/elf.rs`, 108 declarations) | **Production.** Handles `--push-state`, single-dash long options. |
| argv[0] multi-call dispatch (`args.rs:251`) | **Exists.** `ld`→ELF, `ld64`→Mach-O. `"link"` currently `bail!`s at `args.rs:242`. |
| Mach-O backend | **Skeleton.** 40 `todo!()`, arm64-only, feature-gated off, links trivial programs only. Missing `__unwind_info` construction entirely. |
| **Wasm backend** | **~6,650 LOC + 2 CI jobs — inherited, and deleted in P0 (D14).** The original plan was unaware it existed. |
| PE/COFF backend | **Nothing.** |
| Incremental linking | **Nothing implemented.** wild has a *published design* (Nov 2024) that is still unimplemented as of 0.9.0 — read it before I0. wild's phase-ordered immutable-borrow architecture is hostile to it. |
| Test harness (`integration_tests.rs` 7k LOC + `linker-diff` 10k LOC) | **Excellent, ELF-only by construction, and its non-ELF failure mode is *silently green*** (R30). |

Honest read: the fork buys Linux, the arg parser, and — most valuable — a differential test
methodology. It does not buy Mach-O, and Windows is a greenfield backend.

**Note on "wild's `DESIGN.md`" vs "reld's `DESIGN.md`."** These are different documents and the
plan cites both. Every bare reference in the phase docs means **reld's**, in the repo root,
unless it explicitly says otherwise.
