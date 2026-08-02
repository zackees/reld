# reld — Design

> **Status: design document. No implementation exists yet.**
> Every performance figure in this document is a *target*, not a measurement.
> Nothing here has been benchmarked because there is nothing to benchmark.

## 1. Position

`reld` is a successor to [`wild`](https://github.com/wild-linker/wild).

`wild` is the best-architected fast linker in existence. It is the only one that has paid the
cost of a genuine format abstraction: a `Platform` trait with three live backends and a layout
core that is format-neutral in fact, not just in aspiration. `reld` starts from that insight
rather than re-deriving it.

Where `reld` diverges is scope and priority:

| | `wild` | `reld` |
|---|---|---|
| Primary target | Linux / ELF | Linux, Windows, macOS — co-equal |
| Incremental linking | on the roadmap | **the architecture** |
| Optimizes for | the final link | the **edit → link → run** loop |
| Release-quality output | required | **explicit non-goal** |

The last row is what makes the rest affordable. `wild` must eventually be correct enough to
link a distribution. `reld` must be correct enough to link *your machine, right now, on the
200th rebuild of the day*. Dropping the release bar defers LTO, and removes PGO/BOLT
interaction, reproducible-build determinism, and the long tail of compatibility that has
historically consumed years — and killed prior efforts outright.

## 2. Goals

### 2.1 Platform parity

Windows, Linux, and macOS are first-class from day one. Not "Linux plus ports."

| Platform | Object format | Notes |
|---|---|---|
| Linux | ELF | Baseline; the format `wild` already does well |
| Windows | PE/COFF | `x86_64-pc-windows-gnu` and `-gnullvm` first, MSVC after |
| macOS | Mach-O | See §5 on the honest value case |

### 2.2 Incremental linking

The defining feature. A single-object change must not re-run a whole link.

The image is **reflowed**, not rebuilt: the symbol graph and section-contribution graph persist
between invocations, and a changed input patches the graph rather than repopulating it.

**Target: warm relink in the low single-digit milliseconds for a one-object change**, largely
independent of total binary size. This is the number that matters; it is the only one a
developer feels 200 times a day.

### 2.3 Faster than `wild` in every category

This is a target, and to be meaningful it needs a falsifiable definition. `reld` claims
success only when, on identical hardware and identical inputs, it is faster than `wild` on
**all** of:

1. **Cold link** — no cache, no daemon, first run
2. **Warm full link** — daemon resident, all inputs changed
3. **Warm incremental link** — daemon resident, one object changed
4. **Peak RSS** — memory is a category too; winning on time by spending unbounded memory is not winning
5. **Single-threaded** — the honest category, and the one mold *loses* to lld. Parallel scaling must not be the only story

Category 3 is where the architecture should produce a step change rather than a percentage.
Categories 1 and 5 are the hard ones, and where this claim is most likely to fail first —
they are stated explicitly so that failure is visible rather than quietly dropped.

**No benchmark is published until it is real, reproducible, and run against `wild` at a pinned
commit.** See §6.

## 3. Non-goals

Stated as prominently as the goals, because they define the project:

- Release / shipping builds
- Byte-for-byte reproducible output
- Drop-in `ld` replacement for distro build systems
- Winning a cold, from-scratch link against a batch linker on its own terms
  (desirable per §2.3, but not what the project is *for*)

If you need any of these, use `lld`, `wild`, or your platform linker. `reld` is for the inner
loop.

### 3.1 Stretch goals — deferred, not rejected

**(Thin)LTO is a stretch goal, not a non-goal.** It is deliberately sequenced *after* the fast
linker across all target platforms, because a fast non-LTO linker is what the inner loop
actually needs and because LTO interacts with almost every other subsystem — sequencing it
early would slow down the thing the project exists to deliver.

Until it is implemented, LTO flags (`-flto`, `--plugin`, `/LTCG`) must be **rejected or ignored
with a clear, specific diagnostic** — never silently mislinked. Note that `wild` already has a
linker-plugin LTO implementation with known issues; the fork inherits it, so on ELF the starting
position is "partially works", not "absent". Do not delete that code.

## 4. Architecture

### 4.1 Format abstraction

Follow `wild`'s approach: a format trait with per-format backends, and a layout/symbol-resolution
core that never names a format-specific type.

The precedent to respect here is negative. LLD began as a genuinely unified multi-format linker
(the atom model) and **deleted that abstraction in 2021 — 244 files, 36,441 lines**. Today its
four backends share ~2.5% of their code and zero cross-includes. Its developers describe it as
"several different linkers in one binary."

`wild` is re-attempting unification in Rust, where monomorphized traits weaken LLD's
runtime-cost objection. `reld` is betting the same way, with eyes open:

- Abstract the **layout, symbol resolution, GC, and string merging** — these are genuinely
  format-neutral, as `wild` has demonstrated in practice.
- Do **not** abstract relocation processing, section semantics, or output writing. Those are
  where LLD's abstraction earned its deletion.

### 4.2 Incremental model

- Persistent daemon holding the symbol graph and section-contribution graph hot between edits
- Content-addressed input identity, so an unchanged object is never re-parsed
- Patch-in-place layout: reassign only what moved
- Fall back to a full link whenever the incremental path is uncertain — **correctness beats
  cleverness, and a silent wrong answer is worse than a slow one**

### 4.3 Parallelism

Both `mold` and `radlink` demonstrate the same shape: parallel input parsing, a lock-free
symbol table, parallel relocation and image write. `radlink`'s 4-way CAS hash trie
(`lnk_symbol_table.h`) is a good reference implementation and is MIT-licensed, so its
*algorithms* may be studied and reimplemented freely.

Note the caution from §2.3 category 5: mold is *slower than lld single-threaded*. Parallelism
must be the multiplier on an already-good serial path, not a substitute for one.

## 5. Honest risks

Recorded here so they are not rediscovered later as surprises.

- **macOS has weak value on speed.** Apple's `ld_prime` (Xcode 15+) measured **1.4–2× faster
  than `ld64.lld`** on an M4 — benchmarked by `wild`'s own Mach-O lead. `sold`, the commercial
  fast Mach-O linker, was abandoned by the author of both lld and mold, whose README now says
  "Xcode 15 or later are shipped with Apple's new faster linker. I recommend using that
  instead." The honest macOS pitch is **incremental linking and an open implementation**
  (Apple removed `ld64` from Xcode 27, leaving only a closed binary) — *not* raw throughput.
- **Per-format cost is ~1.5–2 person-years to production**, by expert practitioners. Three
  formats is not three times one; it is three times one plus the abstraction tax.
- **The win may be small.** On a 46 MB debug Rust binary on `x86_64-pc-windows-gnu`,
  `ld.bfd` and `ld.lld` measured statistically identical (~1.4 s rebuild, ~0.21 s cargo floor).
  If the entire link budget on a target is ~1 s, an infinitely fast batch linker saves under a
  second. **This is the strongest argument that incremental — not throughput — is the product.**

## 6. Benchmarking policy

- Every published number is generated by CI, from a committed harness, on pinned competitor
  versions
- `wild` is the reference competitor; `lld` and the platform linker are included for context
- Results and the rendered graphic are auto-generated and published to a dedicated benchmark
  branch, and embedded in the README from there
- Categories are those in §2.3. **A category that regresses is still published.** Selective
  reporting is how linker benchmarks lose credibility — mold's own README table overstates its
  advantage over lld by roughly 2× relative to independent measurement, and that is a mistake
  worth not repeating

## 7. Prior art

- **[wild](https://github.com/wild-linker/wild)** — the base and the reference. MIT/Apache-2.0.
  Format-neutral core (~17k lines) with ≤4 ELF references; `Platform` trait with ELF, Mach-O,
  and Wasm impls. Incremental linking sits **above** Mach-O and Windows on its own roadmap.
- **[mold](https://github.com/rui314/mold)** — ELF-only by construction; ~762 ELF constants
  outside its format header, `Chunk<E>` embeds `ElfShdr<E>` by value. Author declined PE/COFF.
- **[radlink](https://github.com/EpicGamesExt/raddebugger)** — shipping parallel PE/COFF linker,
  MIT. ~950 hardcoded `COFF_*`/`PE_*` references and no format abstraction, but an excellent
  reference for parallel COFF parsing and lock-free symbol tables.
- **[lld](https://lld.llvm.org/)** — the cautionary precedent for unification (§4.1).

## 8. Related

- [`soldr`](https://github.com/zackees/soldr) — build tool with content-addressed caching.
  `soldr` makes the compile fast; `reld` is the other half of the loop.
