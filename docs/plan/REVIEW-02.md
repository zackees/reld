# Adversarial review 02 — incremental soundness, test oracle, Mach-O

Second batch of reviewers. `REVIEW-01.md` covers internal consistency, wild-fork feasibility,
and PE/COFF.

**This batch contains the single most consequential finding of the whole review: the phase we
had scheduled as "the first shippable win" targets ~5% of link time.**

---

## R25 — I2 targets the wrong 5% ⚠️ CRITICAL — invalidates the phase as specified

Published phase breakdown for lld on a clang RelWithDebInfo link (MaskRay, 2026):

| Phase | Share |
|---|---|
| **Input parsing** | **5.04%** |
| **Merge / finalize input sections (string merging)** | **66.54%** |

I2 was specified as "a daemon holding parsed inputs hot; re-parse only changed inputs, then redo
layout and write from scratch." **That skips the 5% and repeats the 66%.**

Independently corroborated by wild's own benchmark page:

> "Clang with debug info has massive quantities of debug strings that need to be deduplicated.
> **So link time here is dominated by how quickly we can deduplicate strings.**"

Two linkers, two independent measurements, same conclusion — and specifically on the
debug-info-heavy builds that are reld's entire stated use case.

**Consequence:** the daemon's justification must be rebuilt around **caching the merged string
table and its dedup index**, not cached parses. That is a different, harder and far more
valuable phase. It is also a much worse invalidation problem: one changed object can perturb
global merge offsets, and the merged table is exactly the structure R27's fingerprint cannot
verify.

Note the historical rhyme — mold's `design.md` describes string interning in the preload stage,
i.e. **mold built the caching mechanism this needs, shipped it as `--preload`, and deleted it in
1.3.0** with no published number justifying either decision.

**Required before I2 is scheduled:** run `mold --perf`, `lld --time-trace` and wild's own timing
over reld's corpus and publish the phase split. If parse is ~5% on our workloads, I2 as written
should not be built at all.

**Provenance caveats, recorded so they are not relitigated:** wild publishes no intra-linker
phase breakdown anywhere (the MaskRay table is the only published wild phase data); no per-phase
decomposition of mold's 1.52 s Chromium link exists; no independent measurement of gold
`--incremental` exists beyond Ueyama's "almost 30 seconds" remark; and wild's Feb 2024 figures
(mold 7.539 s, lld 1.602 s) are **end-to-end `cargo run` wall times, not link times** — do not
cite them as link times.

---

## R26 — String merging is also a soundness hole in the fallback gate ⚠️ CRITICAL

The I1 resolution fingerprint hashes `(symbol name, defining input id, value flags)`. It is
structurally **blind to `SHF_MERGE|SHF_STRINGS` offset shifts**: an edit that changes only string
content changes no symbol binding, so the fingerprint says "safe to patch" while every merged
string offset downstream has moved.

The plan is silent on string merging across all of I0–I7. Given R25, that silence spans both the
largest performance term and a correctness hole in the safety gate.

---

## R27 — Other fingerprint blind spots

Changes that alter binary correctness without changing the fingerprint: section contents changing
without symbol changes, alignment changes, COMDAT leader changes where the winner keeps the same
name and file, symbol size changes, and changes to anonymous data referenced only via
section-relative relocations.

The fingerprint needs to cover section content hashes and alignment, not just symbol bindings.

---

## R28 — The daemon's cold-start path reintroduces the cost it exists to avoid

I2 says "on-disk state is the cold-start path, a snapshot the daemon loads in bulk." But §0.4's
whole argument is that loading state proportional to binary size is the fatal fixed cost
(HP-UX's 11.3 s floor, gold's ~30 s null link). A bulk snapshot load is exactly that cost.

The honest position: the snapshot buys *robustness* (surviving daemon death), not speed, and the
millisecond target only holds for warm-daemon links. Say so rather than implying otherwise.

---

## R29 — 25% slack is unbudgeted against the binary it must apply to

Debug info is ~80% of a typical executable. If slack applies to every section including debug
sections, 25% slack on a 2 GB debug binary is ~500 MB of padding — with a direct interaction
with `DESIGN.md`'s peak-RSS win category. The plan never states whether debug sections get slack.

---

# Test-oracle findings

## R30 — `reld-diff` on COFF/Mach-O is new construction, and its failure mode is silently green ⚠️ CRITICAL

`header_diff.rs:249-279`, the per-section field comparison:

```rust
match object.file {
    object::File::Elf64(elf_file) => { /* insert alignment/link/flags/type/entsize */ }
    _ => {}
}
```

For COFF or Mach-O input the arm falls through, `values` is empty, empty-compares-to-empty →
**no diff, test passes.** Mach-O coverage in the entire 10,195-LOC crate is 16 lines; COFF is
**zero references**.

So P1-T3a's "~700 LOC port" is the size of the *ELF* implementation. COFF and Mach-O field
tables are new code — and until they exist, the oracle reports green on formats it never
examined. That is precisely the anti-pattern `08-ACCEPTANCE.md` §6 warns about.

`asm_diff.rs` is worse. Zero `macho|Coff` hits; `section_map.rs:265-271` hard-bails
`"get_elf_section called for non-ELF file"` and is called from **11 sites**; the return type
`ElfSection64<'data,'_,Endianness>` is baked into signatures across all 4,062 lines.

**And the COFF addend problem is confirmed.** `object`'s COFF reader sets
`implicit_addend: true` with `addend` holding only the fixed PC bias (`REL32 => -4`,
`REL32_1 => -5`, …); the real addend lives in the section bytes. `asm_diff` calls `rel.addend()`
at six sites and treats it as the complete symbolic offset. On COFF **every recovered referent
would be wrong**, producing a flood of false positives rather than a clean error.

Additionally, the relaxation-inference machinery that is 60%+ of the file's complexity is dead
weight on COFF (no linker relaxations to infer) and needs a different candidate set on Mach-O —
which also needs `X86_64_RELOC_SUBTRACTOR` pairs and `ARM64_RELOC_ADDEND` (an addend delivered
as a *separate relocation record*), neither expressible in the current chain model.

**Realistic size: 1,500–2,500 new LOC per format**, sharing report plumbing and little else.

## R31 — The `.layout` oracle is symbol-name-keyed, and coverage is far lower than claimed

The actual mechanism: build `symbol_name → (file, section, offset)` from our layout, then look
that name up **in each output binary's symbol table** and record its address. There is no other
mechanism for locating the reference linker's output.

Consequences the plan does not account for:

| Condition | Effect |
|---|---|
| Two objects define a same-named local (`static void log`) | **both dropped** (`section_map.rs:150-153`), section never diffed |
| `--gc-sections` removes it in either linker | dropped by `retain` (`asm_diff.rs:2841`) |
| Output stripped / `--discard-locals` | every local-only section dropped |
| COMDAT/ICF folds two functions to one address | **false positive** (`verify_consistent`, `asm_diff.rs:2921`) |
| `SHF_MERGE` section | skipped |
| Section kind ∉ {Text, Data} | skipped — **no `.rodata`, `.bss`, `.init_array`, TLS** |

`SUPPORTED_SECTION_KINDS` excludes `.rodata`, where most *absolute* relocations live. Estimated
coverage of relocations actually diffed: **~50–70% for C**, materially worse for C++ and Rust.

`linker-diff` has a `--coverage` flag precisely because upstream knows this. **The plan never
requires coverage to be measured or gated — the single cheapest fix available.**

**Mach-O breaks the keying scheme entirely:** `linker-layout::Section` is keyed by input section
index with a single `mem_range`, but Mach-O uses **subsections-via-symbols** — one
`__TEXT,__text` splits into N disjoint output ranges. The schema cannot represent that.

Also: P1-T2's premise is a misreading. `linker-layout` contains zero ELF types and is already
format-agnostic; the real defect is index-keyed-single-range, which the plan does not name.

## R32 — The directive freeze is arithmetically and logically impossible

Measured: **77 distinct directives in use across 247 annotated fixture files. 164 files (66%)
use at least one directive outside the frozen set.** The frozen block also lists **20** names,
not 18 — the plan's own count is wrong.

Two hard contradictions inside a single phase:

- **`ReferenceLinkers:` (126 uses) is dropped**, yet P1-T1's own rationale cites it as the
  replacement for the deprecated `SkipLinker:`. Without it there is no way to say "diff against
  lld but not bfd," which `08-ACCEPTANCE.md` §2 requires. All 126 fixtures would need splitting
  into per-linker configs.
- **`Malfunction:` is required by P1-T5 and is not in the frozen set** — so P1-T5's own fixtures
  fail to parse under P1-T1's reject-unknown parser.
- **`RELD_VERIFY_PLATFORM_REQUIREMENTS` is meaningless without the `Requires*` family**, which
  is also dropped. The escape hatch would have nothing to verify.

`LinkerScript:` (29), `Compiler:` (28) and `ExpectSectionBytes:` (13) have no expressible
equivalent — ~70 fixtures would have to be deleted.

**"Freeze at 18" and "wild's 213 fixtures pass in P2-T1" are mutually exclusive.** Honest fix:
freeze at ~30, including `ReferenceLinkers`, `Requires*`, `Malfunction`, `ExpectDynSym`.

## R33 — mold's skip-list ratchet is broken by mold's own `skip()`

`common.inc:126-130`: `skip() { echo skipped; trap - EXIT; exit 0; }` — **a skipped mold test
exits 0 and therefore "passes."** Combined with `verify_skipped_mold_tests_still_fail` failing
when `output.status.success()`, any skip-listed test that *skips* on our runner (no musl, no
ifunc, probe failure) **spuriously fails the ratchet**, reporting "remove from skip list" for a
test that never ran.

This is exactly the silent-skip anti-pattern `08-ACCEPTANCE.md` §6 warns about, adopted
wholesale. Also: 111 of the 518 are `arch-*` tests requiring qemu, which P1-T8 bans — the honest
runnable count is **407**, and the 111 need explicit exclusion from the ratchet.

## R34 — The fake-linker mechanism is described wrong and works on one platform

The plan says "symlinks named `ld`, `ld.lld`, `mold`." wild explicitly does **not** do this
(`external_tests/mod.rs:126-134`):

> "Note, we can't just create a symlink, since lld requires that it's invoked as `ld.lld` to
> work properly. Instead, we create a wrapper script."

`08-ACCEPTANCE.md` repeats the symlink claim for **all four targets** including `link.exe`.
Windows has no `#!/bin/bash`; `-B<dir>` is a GCC-driver flag `cl.exe` does not accept; `link.exe`
is selected via `LINK`/`/LINKER:`. **The mechanism described as working "across all four" works
on exactly one.**

Also: wild obtains the mold suite via a **git submodule** (`external_test_suites/mold`), which is
uninitialized in our checkout. reld has no `.gitmodules`, and neither P1-T6 nor `08-ACCEPTANCE`
says how the tree is obtained or pinned. A **second** submodule, `wild/tests/bins` (prebuilt
binary test inputs), is depended on by a chunk of the 213 fixtures and is never mentioned.

## R35 — The ignore ratchet needs cross-job aggregation the plan doesn't describe

`should_ignore` (`lib.rs:729-746`) discards the diff with no record of which entry matched. Good
news: hit-tracking is ~20 lines, so the plan's "doubling CI time" fear is unfounded. Bad news:
ignores are glob prefixes applied across fixtures × arches × reference linkers, and
`apply_wild_defaults` installs **different sets per architecture**. An entry is "needed" only
over the union of the whole matrix — including separate macOS and Windows CI jobs. Without a
cross-job aggregation artifact the check is either a no-op or a flake source.

(Also: the actual hard-coded ignore count is **82**, not ~60.)

## R36 — Malfunction coverage is ELF-only, and there is no relocation-application site

All five injection sites are in `elf_aarch64.rs`, `elf_writer.rs`, `elf_x86_64.rs`. Four of five
are *relaxation-suppression*, meaningful only to an oracle that infers relaxations. **There is no
injection site in relocation application** — so the plan's instruction to "port the sites in
relocation application" has no upstream exemplar.

Two gaps: `cfg!(debug_assertions)` means the machinery **compiles out in a release-profile CI
job** (and the CI matrix never states a profile); and at the close of P3 and P4 the COFF and
Mach-O oracle paths are **completely unvalidated** unless new sites are added. Require ≥1
malfunction site per format as a phase-exit gate.

## R37 — Missing from the testing plan entirely

- **Self-determinism is untested, and D12's wording will cause someone to reject the test.**
  D12 rules out *cross-linker* byte reproducibility (correct). It does not rule out: same reld,
  same inputs, N runs, `--threads 1` vs `--threads 16` → identical bytes. Upstream has a dozen
  "tie-break for determinism" comments (`elf.rs:863`, `:4700`, `gdb_index.rs:498`,
  `layout.rs:1239`…), each a latent race with no regression guard. For a *parallel incremental*
  linker this is the highest-probability class of shipped bug, and no layer L1–L5 fires on it.
- **No performance or RSS regression gate** — for a linker whose pitch is the dev loop, a 3×
  link-time regression passes every gate in the document.
- **No "no output on failure" assertion.** A linker that writes a truncated binary and *then*
  exits nonzero passes every gate, and `make` will treat the target as up to date. One line.
- **No scale test.** >64K sections (COFF `NumberOfSections` is `u16`), >2 GB output, >64K
  relocations per COFF section — exactly the class that ships to users and never appears in a
  100-seed synthetic run.

---

# Mach-O findings

Delivered as 20 findings (4 CRITICAL / 7 HIGH / 6 MEDIUM / 3 LOW). Load-bearing items:

## R38 — `__unwind_info` must be **built by the linker**, and P4 never mentions it

Compact unwind is not `.eh_frame`. The linker consumes `__compact_unwind` input sections and
**constructs** `__unwind_info`, with hard structural limits: 511 regular / 1021 compressed
entries per second-level page, a 3-personality cap, a required sentinel entry, and a rule that
LSDA-bearing entries cannot be folded. It appears nowhere in Phase 4 — only in the incremental
doc. Without it, C++ exceptions do not unwind.

The linker must also copy `__eh_frame` into `__TEXT,__eh_frame` for the MODE_DWARF hint.

## R39 — Kernel- and dyld-enforced requirements P4 omits

Verified against xnu `mach_loader.c` and dyld source:

- **`MH_PIE` is mandatory on arm64** — `pie_required()` returns TRUE for `CPU_TYPE_ARM64`;
  otherwise `LOAD_FAILURE`.
- **`__PAGEZERO` must be exactly `vmsize = 0x100000000` with `initprot = 0`**, or `LOAD_BADMACHO`.
  So `__TEXT` starts at 0x100000000.
- **`MH_DYLDLINK` is mandatory** — an arm64 `MH_EXECUTE` without it is `LOAD_FAILURE` on a
  release kernel. **Static arm64 executables do not run.** Hello world must be dynamic.
- **`LC_MAIN`, not `LC_UNIXTHREAD`** — the pre-LC_MAIN path is compiled out except on x86_64
  macOS; an arm64 `LC_UNIXTHREAD` binary maps, fixes up, then `halt("main executable is missing
  LC_MAIN")`.
- **`LC_DYLD_CHAINED_FIXUPS` must point at a valid header even when there are no fixups** —
  datasize=0 is not acceptable, unlike the export trie which may legitimately be empty.
- `LC_ID_DYLIB` in an executable is a hard error; missing `LC_LOAD_DYLIB` is a hard error.

## R40 — `MH_HAS_INIT_OFFSETS` does not exist

Static-initializer dispatch is by **section type**, not a header flag: `S_MOD_INIT_FUNC_POINTERS`
(0x09), `S_INIT_FUNC_OFFSETS` (0x16). For a from-scratch linker `__mod_init_func` is the more
compatible choice. Rust hello-world emits no initializer section at all.

## R41 — Three incompatible dylib-ordinal encodings

`BIND_SPECIAL_DYLIB_*` are `SELF=0, MAIN_EXECUTABLE=-1, FLAT_LOOKUP=-2, WEAK_LOOKUP=-3`, but:

- **opcode binds** use `BIND_OPCODE_SET_DYLIB_SPECIAL_IMM | (ordinal & MASK)`;
- **chained fixups** use an 8-bit field sign-extended only *above 0xF0* — encode `0xFF`/`0xFE`/`0xFD`;
- **nlist `n_desc`** uses `EXECUTABLE_ORDINAL=0xFF`, `DYNAMIC_LOOKUP_ORDINAL=0xFE`, unrelated to
  the signed values.

A real trap for a from-scratch implementation.

## R42 — P4-T5 and P4-T7 contradict each other via rustc's deployment target

P4-T5 says reject the legacy rebase/bind path. But chained fixups are the *default* only above a
deployment-target threshold, and rustc's default `aarch64-apple-darwin` deployment target is low
enough that the legacy path may be selected. Sources disagree on whether ld64's threshold is
macOS 11 or 12.

**Resolve by measurement on the pinned reference toolchain** (`otool -l` on a real `cargo build`
output) before committing to "reject the legacy path."

## R43 — `-dead_strip` is passed by rustc on every default `cargo build`

`GccLinker::gc_sections`: `if self.sess.target.is_like_darwin { self.link_arg("-dead_strip"); }`.
clang does *not* add it. Ignoring it is **correct** (keeping everything is a valid superset) but
it must be accepted, and P4 lists it nowhere.

## R44 — Acceptance realism on the CI runner

Gate item 2 is "runs on a clean Apple Silicon machine with Gatekeeper at defaults." Gatekeeper
and AMFI are different mechanisms: `spctl` would reject an ad-hoc-signed binary that nonetheless
*runs* fine. The gate should test **AMFI** (does it execute), which is what ad-hoc signing
addresses — not Gatekeeper, which ad-hoc signing does not satisfy and which is not what a
locally-built dev binary faces.

Weakly-sourced and to be verified live: the macos-14 runner's Xcode/SDK versions; whether dyld
hard-errors on missing `LC_UUID`/`LC_BUILD_VERSION`; the arm64 mandatory-signing rule is
verified behaviorally (WWDC20), not from a kernel source read.
