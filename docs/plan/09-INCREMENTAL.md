# Phases I0–I7 — Incremental linking

**Do not start before Phase 4 is green.** Incremental linking on top of a backend that is not
yet correct produces bugs you cannot attribute.

This is the product. Everything before it is table stakes. It also has more documented failures
behind it than any other area of linker engineering, so this document leads with the evidence
and with two contradictions in our own `DESIGN.md` that must be resolved before I2 starts.

---

## 0. The evidence

### 0.1 Two things in `DESIGN.md` that cannot both be true

Raise these before writing incremental code. They are not implementation details.

**Contradiction 1 — peak RSS vs. the daemon.** `DESIGN.md` §2.3 lists **peak RSS** as one of the
five categories reld must win. But every fast incremental result in the literature comes from a
**resident process holding the symbol graph and relocation reverse-index in memory**, and Zig's
node-tree design costs **+11.9% peak memory** even on non-incremental links. Holding a
Chrome-scale graph resident is a large RSS commitment. **Millisecond warm relink and beating
wild on peak RSS may not be simultaneously satisfiable.** Decide which yields, and say so in
`DESIGN.md`, before benchmarks are published that assume both.

**Contradiction 2 — archive semantics.** wild's published incremental design **explicitly
declares archive semantics out of scope**, on the stated grounds that Rust doesn't use them:

> "supporting incremental updates of archive semantics would be a lot of work for very
> questionable benefit towards our use case… Rust code, while it uses archives for rlibs,
> doesn't make use of archive semantics."

**reld cannot make that move.** Targeting C and C++ on three platforms makes archive semantics
unavoidable, and archive semantics are precisely Ueyama's core objection (§0.2). This is the
single largest scope difference between reld and the design it is forking from, and it is not
currently acknowledged anywhere in our docs.

### 0.2 The strongest negative evidence

**Rui Ueyama — author of both lld and mold — evaluated incremental linking and rejected it.**
The most authoritative "no" available, because he wrote the two fastest production linkers
(`mold/docs/design.md`):

> "Making a local change doesn't necessarily result in a local change in the binary level. It
> can easily have cascading effects."

> "If you define `atoi` as a weak symbol… I don't know how to efficiently fix up a binary for
> this case."

> "GNU gold takes almost 30 seconds on my machine to do a null incremental link (i.e. no object
> files are updated) for chrome. It's just too slow."

> "I wanted to make full link as fast as possible, so that we don't have to think about how to
> work around the slowness of full link."

mold also **deleted its one piece of cross-invocation state**: `--preload` was deprecated in
1.2.0 and removed in 1.3.0, and `design.md` now carries the header "we no longer believe that
object preloading is a good idea."

**David Lattimore has partially conceded the point — after 18 months.** wild was *named* for
incremental linking. He published a complete design in Nov 2024. **It is still unimplemented as
of 0.9.0, May 2026**, and the tracking issue (wild#184) remains a placeholder:

> "I thought that I was ready to start on this about a year ago, but it turns out that I
> underestimated how much more there was to get a solid linker… When I started the linker, I
> wasn't expecting to get such good performance with non-incremental linking. Seeing the
> performance that we've gotten has changed the equation a bit in terms of what seems important
> to work on."

**This is the most directly relevant data point we have.** The person best positioned to build
what reld wants to build, and who is *ahead* of us on the base linker, has not reached
incremental in two years — because the base linker consumed the time, and once it was fast the
incentive decayed. Plan for that failure mode explicitly: it is the one most likely to happen to
us.

**Every shipped in-place-patching incremental linker in history is dead or deprecated:** Sun
`ild` (never ported to amd64), HP-UX `+ild`, gold `--incremental` (never finished; gold itself
deprecated in binutils 2.44, Feb 2025), Apple prebinding (replaced by the dyld shared cache),
MSVC `/DEBUG:FASTLINK` (removed in VS 2026), SN Systems `rld` (archived Jul 2025). MSVC
`/INCREMENTAL` survives but is **off by default in Chromium release builds and in Unreal
Engine**, whose own build tool comments that it "tends to behave a bit buggy."

**And the competing strategy demonstrably won.** lld, mold, radlink and Apple's ld-prime all
chose "make the full link approach the I/O floor via parallelism." mold links Chromium in 1.52s
— about 2× `cp` of the same output.

### 0.3 Why we do it anyway

1. **Ueyama's objections are about the resolution fixpoint and about detection cost — not about
   the copy cost**, which he concedes is what incremental saves. Both are answerable with a
   *conservative, cheap fallback gate* (I1) rather than with cleverness.
2. **gold's 30s null link is an argument against on-disk state, not against incremental** — see
   §0.4, which is the decisive structural fact in this document.
3. **Our lever differs from Zig's.** `cargo` and `make` already know which translation units
   changed — usually one or two out of thousands. We do not need to diff opaque objects
   cleverly; we need to notice cheaply that 3,000 of 3,002 inputs are byte-identical.
4. **The opening is real and time-limited.** wild has shifted to cold-link performance with no
   timeframe for incremental. That gap is reld's entire reason to exist, and §0.2 shows it can
   close.

### 0.4 The decisive measurement — the fixed-cost floor

From the HP-UX incremental linker paper (USENIX WIESS 2000), on a 118.71 MB / 1,717-object C++
application:

| # objects changed | 2 | 4 | 6 | 8 | 10 | 30 |
|---|---|---|---|---|---|---|
| incremental link time | **11.3 s** | 11.8 s | 13.0 s | 13.1 s | 13.9 s | **16.7 s** |

**Read the shape, not the speedup. Changing 15× more objects costs only 48% more time.** Their
explanation, verbatim:

> "Regardless of the amount of code modified, the time spent on extracting information from the
> output file will always be the same."

A file-persisted incremental linker carries a large **fixed cost proportional to binary size,
not to change size** — it must reload state and rebuild indices before doing any useful work.
Corroborated everywhere: gold's null link on Chrome ~30s; HP-UX's 11.3s floor; Sabatella's 1991
finding that a file-persisted design matched a memory-resident one in the easy case and lost
badly on overflow; and Zig — the only system reaching 65ms — keeping everything in memory and
never reloading.

**Therefore `DESIGN.md`'s target — low-single-digit-millisecond warm relink, largely independent
of binary size — is flatly unachievable with on-disk state. The daemon is a precondition, not an
optimization.** It is also the single architectural choice that genuinely distinguishes reld
from wild, which plans no daemon and will inherit this floor.

**Corollary: publish null-relink time as a headline metric.** It is the number that exposes
fixed cost, and it is the number gold failed on.

---

## I0 — Preconditions (build these DURING P2–P4, not after)

- **I0-T1 — Determinism everywhere** (global rule 4). You cannot diff two layouts that differ
  for reasons unrelated to the edit.
- **I0-T2 — Content-addressed input identity.** BLAKE3 per input, plus `(path, size, mtime)` as
  a fast-path guard. Note the constraint from I2: mold measured `stat(2)` over every Chromium
  input at **under 100ms** and used that to reject inotify. At a 1–5ms target that is 20–100× the
  entire budget, so **file-watching becomes mandatory rather than optional**, and wild's planned
  mtime-scan detection is already too slow for our stated goal.
- **I0-T3 — Stable IDs.** wild has `input_section_id.rs`, `SectionIdRange`, `SymbolIdRange`.
  Preserve this substrate. Note wild's design calls out needing a **global** input-section ID
  (it currently indexes per-file only) — add that now.
- **I0-T4 — Layout journaling.** Extend the `.layout` sidecar built for the test oracle (P1-T2)
  into a complete, reloadable record. The harness and the incremental engine want the same data.
- **I0-T5 — `--verify` mode.** Re-link from scratch; compare symbol addresses and section
  contents against the incremental result. Cheap now, expensive to retrofit.
- **I0-T6 — No global mutable state, no `exit()` on error.** The lld-as-a-library RFC stalled on
  exactly two blockers: fatal errors exiting the process instead of returning control, and
  global-variable conflicts between threads. A daemon-hosted linker cannot have either. Design
  it out now; retrofitting is what killed that RFC.
- **I0-T7 — `reld log`, always on.** One line per invocation: full or incremental, and **if
  full, the numbered trigger from I1**. Silent, unexplained fallback destroyed every prior
  system's reputation and is the substance of Ueyama's third objection. wild plans this; Sun
  `ild` did it. Adopt it before the first fallback exists.

`Acceptance:` `reld --verify` passes on every link in the P2–P4 gates; `reld log` emits a line
per invocation.

---

## I1 — The fallback gate (build before any patching exists)

**Two rules govern this phase:**

1. **A discovered fallback is slower than never having tried.** Microsoft documents that a
   fallback full link is slower than a plain full link "due to the time spent determining
   fallback is necessary." Therefore **groups A, B, C and F below must be evaluated before any
   work is done, from cheap metadata only.** Only D and E are legitimately discovered mid-link.
2. **Every fallback is logged with its trigger number** (I0-T7).

### The trigger checklist

Consolidated from MSVC, Sun `ild`, HP-UX, gold and wild. Treat as a specification.

**A. State integrity** — 1 state missing · 2 state unreadable/unwritable · 3 state corrupt or
truncated (classic cause: a build cancelled mid-link) · 4 previous link failed or was
interrupted · 5 linker version or state schema changed · 6 output binary missing

**B. External mutation** — 7 output altered since last link (`strip`, code signing, antivirus
quarantine, checksum tools) · 8 unexpected timestamp change on output or state · 9 working
directory changed (state records relative paths)

**C. Invocation shape** — 10 any linker option changed · 11 an input object added, removed, or
reordered · 12 linker script / mapfile changed · 13 any unsupported option present at all
(`--gc-sections`, `-r`, `--emit-relocs`, LTO plugins, `/OPT:REF`, `/OPT:ICF`, `/ORDER`).
Note LTO is a deferred **stretch goal**, not a non-goal (`DESIGN.md` §3.1) — when it lands it
will be incompatible with the incremental path (gold and MSVC both fall back for it), so this
trigger stays permanent regardless.

**D. Space exhaustion** — 14 patch space exhausted in any section · 15 input section
incompatible in size or alignment with its recorded slot · 16 **check both per-file and generic
padding before giving up** — HP-UX documents falling back when file-specific padding was
exhausted even though generic section padding had room; do not reproduce that bug

**E. Semantic non-locality** — 17 multiply-defined symbol whose new winner cannot be determined
locally · 18 COMDAT group removed from the object that contributed it · 19 archive membership
changed · 20 a strictly-ordered section modified (`.init`, `.init_array`) · 21 a weak/strong
override relationship changed (Ueyama's objection; a special case of 17)

**F. Economic** — 22 **"too many files changed."** Sun `ild`: "When a high percentage of object
files change, ild determines a full relink is faster." This is the only documented *predictive*
fallback heuristic in the entire corpus. Implement one.

### The resolution fingerprint

Triggers 17–21 are detected by one mechanism: a hash over the sorted set of
`(symbol name, defining input id, value flags)` bindings, plus the archive-member selection set
and the undefined set. If it changes, the cascade Ueyama describes has occurred → full link.

⚠️ **The fingerprint as originally specified has blind spots** (R26, R27). It must additionally
cover:

- **Merged-string content.** It is structurally blind to `SHF_MERGE|SHF_STRINGS` offset shifts:
  an edit changing only string content changes no symbol binding, so the gate says "safe to
  patch" while every merged offset downstream has moved. This is the same subsystem that IX-T0
  identifies as ~66% of link time — it is simultaneously the largest performance term and a
  correctness hole in the safety gate.
- **Section content hashes and alignment** — contents can change with no symbol change.
- **COMDAT leader identity** where the winner keeps the same name *and* file.
- **Symbol size**, not just address and binding.
- **Anonymous data** referenced only via section-relative relocations, which has no stable name
  to key on. (wild's published design hits the same problem and proposes matching such sections
  "by looking at what references them.")

This converts his unanswerable case into a cheap detection problem. **We do not fix it up. We
notice and bail.**

For the sub-case where we *can* proceed, adopt HP-UX's two-copies-per-symbol procedure (their
§5.2) rather than persisting every definition — they measured 130,000 incoming COMDAT groups
(40,000 unique) in one test program and judged full bookkeeping to "negate all advantages of the
incremental linker." Their posture is the right one for us:

> "we have chosen to forgo complex schemes that require vast amounts of bookkeeping in an
> attempt to guarantee incremental linking 100 percent of the time. Instead, we chose
> lightweight schemes that address the majority of situations that occur in practice."

That only works if the fallback is cheap and legible — which is what rule 1 and I0-T7 are for.

`Acceptance:` every trigger has a fixture that provokes it; CI asserts the fallback fired **and**
that `reld log` names the correct trigger number.

---

## IX-T0 — Measure before designing ⚠️ gates everything below

**The original I2 was refuted by review.** It specified a daemon holding *parsed inputs* hot,
then relinking from scratch. Published phase data for lld on a clang RelWithDebInfo link:

| Phase | Share |
|---|---|
| Input parsing | **5.04%** |
| Merge / finalize input sections (string merging) | **66.54%** |

**That design skips the 5% and repeats the 66%.** Corroborated independently by wild's own
benchmark page: *"link time here is dominated by how quickly we can deduplicate strings"* — on
exactly the debug-heavy builds reld targets.

So, before any daemon design is committed (D16):

1. Run `mold --perf`, `lld --time-trace` and wild's timing instrumentation over the P2-T3 corpus
   and the `reld-testkit` workloads.
2. Publish the per-phase split to the `benchmark-stats` branch — **a split that undermines the
   plan is still published**, same policy as the benchmarks.
3. Derive the phase's scope from that measurement; record it as D16a.

If parsing is ~5% on our workloads, **the parse-caching daemon is not built at all.**

The likely real target is **caching the merged string table and its dedup index**. That is both
the dominant cost and a far harder invalidation problem — see the soundness hole below. Note the
precedent: mold's `design.md` describes string interning in its preload stage, i.e. mold **built
this mechanism, shipped it as `--preload`, and deleted it in 1.3.0** with no published number
justifying either decision. Find out why before repeating it.

`Acceptance:` a published per-phase breakdown for reld, mold, lld and wild over the corpus.

## IX — Daemon + warm link (scope set by IX-T0)

A resident process holds hot state across links; on relink it revalidates inputs and reuses
whatever IX-T0 identified as the dominant reusable cost. **Still zero patching** — layout and
write are redone. That keeps correctness risk to staleness alone, which I0-T2 handles.

Design constraints from the prior art:

- **Resident state is the fast path; on-disk state is the cold-start path**, and the on-disk form
  is a **snapshot the daemon loads in bulk**, never something it reconstructs incrementally.
- **Digest-per-input validation**, following Bazel's persistent-worker protocol: the digest
  travels with the input so the daemon validates cached state without re-reading. Assume any
  daemon state not keyed by a content digest is a latent correctness bug, and design so a
  stale-state bug degrades to a full link rather than a wrong binary.
- **Build-system-supplied change sets.** gold shipped `--incremental-changed` /
  `--incremental-unchanged` / `--incremental-unknown` / `--incremental-base=` so an external
  build system declares truth. This removes both the stat-scan cost (I0-T2) and a whole class of
  staleness bug. Nobody has combined **daemon + snapshot + build-system-supplied change set** —
  that combination is the most defensible novel claim available to reld, and it is cheap.
- **A daemon panic is a recoverable event**, degrading to a cold full link in a fresh process,
  never a build failure.
- **The daemon holds an mmap of the output and is therefore a lock-holder.** It must release and
  re-acquire cleanly and detect that the file it mapped is no longer the file on disk. On Windows
  a running process locks its own image (`LNK1104`).
- Bounded memory with an LRU over cached parses. Owner-only socket permissions.
- **Free win while here:** mold's overwrite-don't-recreate output trick — reusing the existing
  output file's blocks rather than creating a fresh one saved ~300ms on a 2 GiB output. It is
  free and not widely known.

`Acceptance:` warm relink after a one-object change is measurably faster than cold; `--verify`
passes on every link; killing the daemon mid-build still produces a correct binary; **null
relink time is published**.

---

## I3 — In-place patching, no-growth case

Legal only when the I1 fingerprint is unchanged and every changed section fits its slot. Patch
bytes in place, re-apply relocations for changed sections, move nothing.

**Slack: 25%.** The evidence converges tightly — Quong & Linton measured **24% ⇒ 97% of updates
in place**, Zig uses 25% (`MappedFile.growth_factor = 4`) and 33% (older allocator, and
`Dwarf.zig`). **gold used 10% and it was insufficient.** Saturating-add, with a minimum slot
size.

`Acceptance:` a same-size function-body edit relinks without moving any symbol; `--verify` passes.

---

## I4 — Growth and movement

**Use pure relocation re-patching. Do not force GOT/PLT indirection and do not build a custom
jump table.** The Zig team tried exactly three strategies in this order:

1. **Force a GOT for everything** (2020), so references never change — abandoned.
2. **A custom embedded jump table** (2024) — abandoned as a "design mistake"; custom relocations
   broke tooling and forced every codegen backend to know about the table.
3. **Pure relocation re-patching on move**, with exponential growth making moves rare — where
   they landed and what current Zig master does.

**Start where they finished.** The four known strategies and their costs: padding (fragile,
needs an overflow story), indirection (needs compiler cooperation or accepted runtime loss —
tried twice and abandoned), trampoline at the vacated slot (MSVC's approach; degrades stack
quality and symbolization), reverse index + re-patch (needs O(relocations) persistent state).

Requirements:

- **Relocation reverse index.** The memory-heavy structure of the design. Do **not** use a `Vec`
  per symbol — wild's design rejects this explicitly as "too expensive" and instead uses an
  **index-based linked list embedded in the relocation array**: a per-symbol head index and a
  per-relocation next index, buildable in parallel via atomic compare-exchange on the heads.
  Adopt that.
- **Deferred fixups.** Mark dirty, resolve when the work queue drains. `moved` propagates down
  to children; resizes bubble up to the parent.
- **Byte insertion via kernel primitives.** `fallocate(FALLOC_FL_INSERT_RANGE)`,
  `FALLOC_FL_PUNCH_HOLE`, `copy_file_range` on Linux, with runtime unsupported-fallbacks; a
  section handle on Windows. **macOS has no `INSERT_RANGE` equivalent — plan the copy fallback
  explicitly.** This is source-only knowledge from Zig's `MappedFile`, undocumented in any post,
  and it is how "insert 400 bytes into a 2 GB image" becomes cheap.

`Acceptance:` a function grown past its slot relinks correctly with all references re-patched;
500 seeded random growth edits stay correct.

---

## I5 — The sorted-table subsystem (design up front, not as a detail of I4)

⚠️ **This is what actually killed gold's C++ support, and it is identical on all three of our
target formats.**

| Format | Table | Constraint |
|---|---|---|
| ELF | `.eh_frame_hdr` | binary-search table sorted by initial location |
| PE/COFF | `.pdata` | `RUNTIME_FUNCTION` entries must be sorted by function address |
| Mach-O | `__unwind_info` | sorted compressed second-level pages |
| all | `.init_array` / `.ctors` | contiguous, no gaps permitted |

These are **global, address-sorted indices over the whole image**. Padding does not help: it
preserves the address of the function that *grew*, but any function that *moves* invalidates the
sort for the entire table. **You cannot pad or indirect your way out of this. The tables must be
rebuilt.**

Documented consequences of getting it wrong:

- gold **never solved it** — `.eh_frame_hdr` was simply disabled under `--incremental` (2012
  future-work item, still disabled in 2016), which is exactly why a user reported the feature
  "virtually impossible" for C++ development. Without `.eh_frame_hdr`, C++ unwinding falls back
  to linear search or fails; on Windows an unsorted `.pdata` means `RtlLookupFunctionEntry`
  fails and there is no unwinding at all.
- gold's adjacent `.debug_info` filler — dummy compile units — **broke `nm` and `addr2line`**
  with `Dwarf Error: Bad abbrev number: 0`. **Format-legal filler is not the same as
  tool-tolerated filler.** Zig's approach is better: genuinely format-defined no-ops
  (`DW_LNE_padding` in `.debug_line`, `DW_CFA_nop` in `.debug_frame`).

Design: maintain a **persistent sorted index keyed by FDE / `RUNTIME_FUNCTION`** so a moved
function is a delete+insert into a sorted structure rather than an N-entry re-sort. First-class
subsystem, budgeted explicitly.

`[Open risk]` Modern Mach-O **chained fixups** encode rebase/bind metadata *in the pointer
itself* as per-segment linked chains. Inserting or moving a fixup site means re-threading a
chain, not writing a slot. No published source discusses incremental patching under chained
fixups. Treat as unexplored risk and prototype early.

`Acceptance:` after 100 incremental edits, unwinding works through moved functions on all three
platforms, and `nm`/`addr2line`/`llvm-symbolizer` still parse the output cleanly.

---

## I6 — Debug info

**80% of a typical executable is debug info.** Coutant's own gold slides note debug strings grew
**~10×** under `--incremental` because deduplication was disabled. **If you do not solve debug
info incrementally, you have solved 20% of incremental linking.**

Everyone foundered here: gold's padding broke binutils tools; MSVC PDBs are append-only and
accumulate roughly 2:1 stale-to-current records over 1,000 builds *even with incremental
disabled*; `/DEBUG:FASTLINK` was removed for producing a worse debugging experience and
non-portable PDBs; **Zig's newest linker emits no DWARF at all and has never emitted PDB**;
radlink invented an entirely new debug format (RDI) rather than fight PDB.

That the leading practitioners twice proposed **DWARF standard extensions** to make this
tractable — `DW_LNS_jmp` (Kelley, 2020) and `DW_LNS_indirect_line` (Lugg, 2024) — is the
clearest signal of how hard incremental line-number information is.

**Decide explicitly, and early: does reld's warm path emit usable debug info, or does "warm
relink" mean "no debug info"?** The second answer makes the product substantially less
interesting, because the dev loop is where people debug. Note our "release-quality output is a
non-goal" framing — well-supported by Sun, HP-UX, gold, MSVC and Live++, all of which say
development-only explicitly — **does not buy us out of this**, precisely because developers
debug.

`Acceptance:` after 100 incremental edits, gdb/lldb resolve every function by file:line,
backtraces are correct through moved functions, and debug-section growth is bounded.

---

## I7 — The bookkeeping wall

A real 37ms Zig incremental update breaks down as: sema ~1.2ms, codegen ~240µs, **linking
~170µs** — and **~31ms in the flush-time reference-graph traversal.** mlugg:

> "the vast majority of the duration of this incremental update is being spent figuring out that
> a graph didn't change!"

**Once patching works, patching is no longer the cost — invalidation bookkeeping is, by ~180×.**
A linker that nails the patch path and then does a naive O(symbols) sweep lands at ~30ms, not
the low single-digit milliseconds `DESIGN.md` targets.

**Invalidation must be O(changed), not O(program)** — a design constraint from I1 onward, not a
late optimization. Instrument from the first incremental commit: a per-phase timing breakdown on
every incremental link, so the wall is visible as it approaches.

`Acceptance:` CI asserts bookkeeping time does not scale with total input count when the changed
set is held at one object.

---

## Acceptance testing for incremental

The reference is **a full link of the same final inputs** — this is wild's own proposed
methodology and the only one that catches the bug class (`LNK1000`, gold returning `NULL` into a
`std::string`) that destroyed prior systems' reputations.

1. **Equivalence oracle.** Incrementally link an edit sequence, then full-link the same final
   input set. Both must **run identically** (exit code, stdout) and expose the same addresses
   for all defined symbols. Not a byte comparison — they need not be byte-identical.
2. **Edit-sequence stress.** Extend `reld-testkit` with a mutation generator: grow a function,
   shrink one, add/remove a symbol, change a weak binding, add an archive member. Seeded random
   sequences; a failure reports one `u64` reproducing the whole sequence.
3. **Fallback assertion.** Every I1 trigger has a fixture; CI asserts the fallback fired and
   `reld log` named the right trigger. An incremental path that silently handles a case it
   should have rejected is the highest-severity bug class here.
4. **Long-run drift.** 1,000 sequential incremental links without a full link, verified at the
   end. Catches slow corruption a single edit will not.
5. **Speed metrics, honestly framed.** Report warm incremental against the **do-nothing floor**
   (compile-only, no link) — Zig's "~4% over not linking at all" is meaningful; a multiple
   against our own previous implementation is not. **Publish null-relink time** (§0.4).

> ⚠️ **Do not cite "11x faster" for Zig anywhere.** That figure was publicly retracted by its
> author — the old linker had been run emitting debug info while the new one emitted none. It is
> circulating in press coverage and is wrong. The correct framing is 65ms vs a 62ms
> typecheck-only floor.

`Acceptance:` `ci/gate-incremental.sh` exits 0 on all four targets.

---

## Strategic risks to revisit at I2

- **Zig's advantage is compiler/linker co-design, not linker cleverness.** They feed the linker
  per-declaration updates and never produce object files. reld consumes opaque `.o` from clang
  and rustc, inheriting the hard problem Zig sidesteps. **Their 65ms is not transferable on its
  own.** Kelley: "you just can't do it on accident. You need to design for it from the
  beginning."
- **Live++ may be solving the same user need more cheaply.** It achieves sub-second iteration by
  patching the **running process** — recompiling individual objects, linking a patch DLL, and
  injecting it — with no image patching at all. It needs no build-system integration because it
  reads the build graph out of the PDB. It ships on Windows, Xbox and PlayStation. **If the
  user's real goal is edit→run latency rather than edit→link latency, that is the competitor to
  beat**, and it is worth understanding why they chose that layer before committing to ours.
- **`lld-inc`** (a COFF incremental linker on lld) claims up to 3× faster than LLD on a Chromium
  benchmark *while preserving bit-exact determinism* — the only work claiming determinism and
  incrementality together. The paper was not retrievable automatically; track it down properly.
