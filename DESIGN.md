# reld — Design

> **Status: reld links real programs on all three platforms today.** Linux/ELF links natively
> (inherited from wild). Windows/COFF and macOS/Mach-O link via a **bridge** to `lld`
> (`rust-lld`, shipped with every Rust toolchain) — see §4.4 and
> [issue #17](https://github.com/zackees/reld/issues/17) for the phase history. Native COFF/
> Mach-O codegen in reld's own engine is still future work. Every performance figure in this
> document is a *target*, not a measurement, except the Linux `reld` column in the published
> benchmark, which is now a real CI-generated measurement (§6). reld's bundling of multiple real
> linkers per platform is the basis for the **polylinker** framing in §4.5. Its initial ELF
> implementation routes LTO, ICF, and compatibility-policy requests to a capable bundled engine;
> see [#30](https://github.com/zackees/reld/issues/30). Native COFF/Mach-O engines and the broader
> capability matrix remain future work.

## 1. Position

`reld` is a source fork of [`wild`](https://github.com/wild-linker/wild), vendored from commit
`5793935f1d8b05b9a978ce2089e16e718072e9a9`.

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

**[SHIPPED routing; native codegen remains future work.]** reld does not plan to implement LTO natively
in its own engine as the primary path. Instead, per the polylinker model (§4.5) and
[#30](https://github.com/zackees/reld/issues/30), an LTO-shaped link request
(`-flto`, `--plugin`, `/LTCG`, `/GL`) is meant to be **routed to whichever bundled engine already
has real LTO support** — today that would be the `lld` bridge (§4.4), which does. This makes LTO
*delegated*, not rejected, at the product level, while reld's own engine still owns zero LTO
codegen. The flag-aware router implements this for ELF today; see §4.5 for the exact capability
set. Forcing the native engine for an LTO request is a clear capability error, never a silent
mislink. Note that `wild` already has a
linker-plugin LTO implementation with known issues; the fork inherits it, so on ELF the starting
position is "partially works", not "absent". Do not delete that code. See
[D13](docs/plan/01-DECISIONS.md) for the locked-decision record of this reframing.

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

### 4.4 Windows/macOS bridge (shipped) vs native backends (future)

Native codegen for PE/COFF (§4.1, §2.1) and Mach-O is not yet built. In the meantime, reld
**bridges** Windows and macOS links by delegating to `lld` — `lld-link` for COFF, `ld64.lld` for
Mach-O — both drivers built into the `rust-lld` binary that ships with every Rust toolchain.
reld discovers the linker, classifies argv for routing, execs it with the compatible linker
arguments, and propagates its exit code and diagnostics verbatim; the bridge does no layout of
its own. This is proven by CI
end-to-end builds on the windows-msvc and macos-arm64 runners that link and run a real
multi-crate program with a C dependency (`rusqlite` bundled/SQLite).

The bridge is not a native backend and does not reduce the scope of §2.1 or §4.1 — it exists so
Windows and macOS *work now* while the native engine (§4.1's format abstraction) is built out.
When the native backend for a platform lands, it becomes the default for that platform and the
bridge remains available for cases the native engine will not cover (e.g. LTO, full release
fidelity). See [issue #17](https://github.com/zackees/reld/issues/17) for the phase history
(BR-1 Windows bridge, BR-3 macOS bridge, BR-4 benchmark integration).

### 4.5 Polylinker: flag-aware routing over bundled engines

**[INITIAL IMPLEMENTATION SHIPPED.]** §4.4 establishes that reld bundles more than one
real linker per platform (native engine + lld bridge) and already routes between them — but only
on the platform/format axis, decided once at dispatch. [#30](https://github.com/zackees/reld/issues/30)
proposes generalizing that into a **polylinker**: a router that also inspects the *requested
configuration* (argv flags, env) and picks the bundled engine whose capabilities cover it. This
section specifies the policy implemented by the initial ELF router (B8 in
[#17](https://github.com/zackees/reld/issues/17)) and to be extended by the daemon router in
[#19](https://github.com/zackees/reld/issues/19).

**Capability model.** Each bundled engine declares, in a static table (plus a cheap runtime
probe where useful), which configurations it supports: LTO/ThinLTO, plugin interface,
GC-sections, ICF, PDB vs. DWARF, identity/reproducible output, incremental linking, and which
object formats (ELF/COFF/Mach-O) it drives. Example shape (illustrative, not a committed API):

```
Engine::Native   { formats: [Elf],                lto: false, incremental: true,  ... }
Engine::LldBridge{ formats: [Coff, MachO, Elf],    lto: true,  incremental: false, ... }
```

**Routing policy.** On each link:

1. Classify the requested configuration from argv (`-flto`, `--plugin`, `/LTCG`, `/GL`,
   optimization level, `--icf`, `--gc-sections`, identity/strip flags) and env.
2. Default to the fastest bundled engine for the platform — the dev-loop path.
3. Escalate to a more capable bundled engine when the requested configuration demands it (e.g.
   `-flto` on a platform where the native engine exists but doesn't support LTO → route to the
   lld bridge instead).
4. `--engine=<name>` / `RELD_ENGINE` bypasses automatic engine selection for benchmarking and
   as an escape hatch; the requested engine is still format- and capability-validated.

**Fallback ordering.** If the default (fastest) engine can't satisfy a requested capability, fall
back to a capable bundled engine rather than failing outright, ordered by (capability match, then
speed). If **no** bundled engine supports the requested configuration, that is a loud,
specific error naming the unsupported feature — routing must never silently drop a
correctness-affecting flag or produce a silent mislink.

**Honesty and logging.** Every routing decision is meant to be logged at note level, in the same
spirit as the bridge's existing `reld: delegating to <linker> (bridge mode)` line (§4.4, B5):
`reld: engine=<name> (reason: default|flag:-flto|fallback:<cap>|override)`. The benchmark harness
already records *which* engine produced a number (§6, the `mode`/`engine` field) — routing
generalizes that same discipline to ordinary links, not just benchmarks.

**Interaction with incremental linking.** LTO and the incremental daemon path (§4.2) are mutually
exclusive on a given link (same as gold and MSVC). The router must select one engine and must not
attempt to combine an LTO-routed link with the incremental path.

**Relationship to native backends.** Routing does not reduce the scope of native COFF/Mach-O
codegen (§4.1, §2.1, #7/#8) or native LTO (§3.1). It is a *product-level* answer — "can reld
satisfy this link request today, using something it already bundles" — independent of how many
of reld's own backends exist. New bundled backends are added by extending the capability table,
not by forking a new routing path per engine; see
[`agents/docs/polylinker.md`](agents/docs/polylinker.md).

**Status summary:**

| Component | Status |
|---|---|
| Multiple real linkers bundled per platform | Shipped (§4.4) |
| Routing by platform/format at dispatch | Shipped (§4.4) |
| Capability table | Shipped: initial ELF/LTO/ICF and compatibility-policy set |
| Flag/config classification + routing | Shipped for ELF, including nested response-file plugin detection |
| Fallback-on-missing-capability | Shipped: native ELF → ELF `lld` |
| `--engine=` / `RELD_ENGINE` override | Shipped |
| Per-decision routing log | Shipped |

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
