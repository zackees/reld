# reld implementation plan — overview

**Audience: the implementing agent.** Every decision that requires judgement has already been
made and is recorded here or in `01-DECISIONS.md`. If you hit something this plan does not
cover, stop and ask rather than improvising — an unplanned improvisation in a linker surfaces
as a miscompiled binary three months later, not as a failing test today.

## Reading order

| Doc | Contents |
|---|---|
| `00-OVERVIEW.md` | This file. Phase map, global rules, definition of done. |
| `01-DECISIONS.md` | Locked decisions (D1–D13) with rationale. Do not relitigate. |
| `02-PHASE-0-FORK.md` | Fork wild, licensing, rename, argv[0] dispatch. |
| `03-PHASE-1-HARNESS.md` | Acceptance-test infrastructure. Built **before** new format code. |
| `04-PHASE-2-LINUX.md` | Prove inherited ELF works under reld's name and CI. |
| `05-PHASE-3-WINGNU.md` | PE/COFF backend, MinGW ABI. The largest phase. |
| `06-PHASE-4-MACOS.md` | Mach-O arm64. |
| `07-PHASE-5-WINMSVC.md` | MSVC ABI + the PDB decision. |
| `08-ACCEPTANCE.md` | Per-platform acceptance criteria and the CI matrix. |
| `09-INCREMENTAL.md` | Phases I0–I5. Do not start before Phase 4 is green. |

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
3. **No `todo!()` / `unimplemented!()` in merged code.** If a path is unsupported, it must
   `bail!()` with a message naming the feature and the input file. wild's Mach-O backend is a
   40-`todo!()` skeleton and is the anti-pattern here: it compiles, reports success, and panics
   at runtime on real input.
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
| P0 | `cargo test --workspace` green; `reld --version` works; wild's own suite passes under the new crate names. |
| P1 | Harness runs on all four platform targets in CI; differential oracle validated against `ld.bfd`; mutation-injection tests prove the oracle catches deliberate bugs. |
| P2 | reld self-hosts on Linux; real-world corpus pass-rate published; benchmark `reld` column populated. |
| P3 | `cargo build --target x86_64-pc-windows-gnu` linked by reld produces a running binary; gdb breakpoint hits. |
| P4 | Rust hello-world runs on clean Apple Silicon with Gatekeeper at defaults. |
| P5 | MSVC-ABI binary runs; debug story resolved per D5. |
| I* | See `09-INCREMENTAL.md` — each incremental phase is gated on a correctness oracle, not a speed number. |

## What we inherit vs. what we build

Measured from the wild tree at `.extern-repos/wild`:

| Component | Status on fork |
|---|---|
| ELF backend (`libwild/src/elf.rs` + `elf_writer.rs`, ~12.5k LOC) | **Production.** 5 architectures. Inherited working. |
| `Platform` / `Arch` traits (`libwild/src/platform.rs`, 1573 LOC) | **Real and load-bearing**, but a 163-member interface extracted from ELF. Expect friction. |
| GNU ld argument parser (`libwild/src/args/elf.rs`, 108 declarations) | **Production.** Handles `--push-state`, single-dash long options. |
| argv[0] multi-call dispatch (`args.rs:251`) | **Exists.** `ld`→ELF, `ld64`→Mach-O. `"link"` currently `bail!`s at `args.rs:242`. |
| Mach-O backend | **Skeleton.** 40 `todo!()`, arm64-only, feature-gated off, links trivial programs only. |
| PE/COFF backend | **Nothing.** |
| Incremental linking | **Nothing.** Not even a design sketch; `DESIGN.md` does not mention it. The phase-ordered immutable-borrow architecture is actively hostile to it. |
| Test harness (`wild/tests/integration_tests.rs` 7k LOC + `linker-diff` 10k LOC) | **Excellent, and ELF-only by construction.** |

Honest read: the fork buys Linux, the arg parser, and — most valuable — a differential test
methodology. It does not buy Mach-O, and Windows is a greenfield backend.
