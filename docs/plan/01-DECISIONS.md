# Locked decisions

These are settled. Do not relitigate them mid-implementation. If evidence emerges that one is
wrong, stop and raise it as an issue — do not quietly implement the alternative.

---

## D1 — Fork wild. **Locked: fork.**

reld is a fork of `wild` (MIT / Apache-2.0), not a from-scratch implementation.

Rationale: Linux/ELF arrives at production quality immediately, so all net-new effort goes to
PE/COFF and Mach-O — the two formats with no open fast implementation. It also gives the
implementing agent a working reference to pattern-match against *inside the repo*, which matters
more than usual here.

Consequence to accept: wild's `Platform` trait is a 163-member interface with 43 associated
types and ~15 `*Ext` escape hatches, in a single 1573-line file, all `pub(crate)`. It was
mechanically extracted from ELF rather than designed. There is no out-of-tree plugin path; a new
backend must be in-tree.

## D2 — Driver surface. **Locked: argv[0] multi-call, extending wild's existing mechanism.**

wild already dispatches on `Path::file_stem()` at `libwild/src/args.rs:251` and on a leading
`-flavor` argument at `args.rs:238`. Extend both:

| Binary name | `-flavor` | Platform | Arg dialect |
|---|---|---|---|
| `ld.reld` | `gnu`, `ld` | ELF | GNU ld |
| `reld-link` | `link` | COFF | MSVC `link.exe` |
| `ld64.reld` | `darwin`, `ld64` | Mach-O | ld64 |

Note `args.rs:242` currently reads `"link" => bail!("Windows (link flavor) is not yet
supported")`. That line is the marker for where COFF plugs in.

**MinGW note:** the MinGW toolchain drives a GNU-syntax `ld`, not `link.exe`. So win-gnu uses
the **GNU dialect with the COFF platform** — the format axis and the argument-dialect axis are
independent, and the code must treat them as such. Select via `-m i386pep` (the MinGW emulation
name) or an explicit `--target`.

## D3 — v0 target matrix. **REVISED after review 01: msvc before gnu.**

| Order | Target | Notes |
|---|---|---|
| 1 | `x86_64-unknown-linux-gnu` | Inherited working |
| 2 | **`x86_64-pc-windows-msvc`** | First new backend; debug info deferred |
| 3 | `aarch64-apple-darwin` | Apple Silicon, not x86_64 |
| 4 | `x86_64-pc-windows-gnu` | After the COFF core is proven |

**The original ordering (gnu first) was wrong and is superseded.** It priced only the
debug-info axis and priced MinGW compatibility at zero. Corrected ledger:

| | windows-gnu | windows-msvc |
|---|---|---|
| Debug info | DWARF, but **COMDAT-grouped `.debug_frame`** | PDB — deferred per D5 |
| Auto-import + runtime pseudo-relocs | **required** (R1) | n/a |
| Default linker-script emulation: ~25 provided symbols, `KEEP` set, `.idata$N` ordering | **required** (R5, R6) | n/a — `link.exe` semantics *are* the spec |
| `.rsrc` | **every link** (`default-manifest.o`) | required |
| `.CRT$X*` + `.ctors`/`.dtors` | **both** | `.CRT$X*` only |
| `.drectve` | not needed | needed |
| In-tree reference implementation | **none** | radlink, 19.7k LOC, exactly this target |
| Spec quality | folklore + binutils `emultempl/pe.em` | Microsoft PE/COFF spec + radlink + lld/COFF |

Findings R1, R5 and R6 are all in the region where radlink gives zero help. Going msvc-first
reduces the net-new-without-reference surface to roughly zero, because radlink carries COMDAT,
weak externals, base relocations, `.rsrc`, TLS, imports/exports and `.drectve` case-for-case.

The COFF **core** (parsing, COMDAT, relocations, base relocations, writer, `.pdata`) is shared
between the two ABIs. windows-gnu then becomes an additive phase: the GNU arg dialect,
auto-import, default-script emulation, `.ctors`/`.dtors`, and DWARF.

Accepted cost: **no debugger on Windows until D5 is resolved.** That is the explicit tradeoff.

## D4 — Input parsing. **Locked: the `object` crate for v0.**

Use `object` for COFF/PE parsing (wild already depends on it for ELF). Hand-rolled zero-copy
readers are a later optimization, gated on profiling evidence, and must not be attempted during
backend bring-up. Correctness first; the format is new to us and `object` is well-tested.

## D5 — PDB. **Locked: deferred to Phase 5, and win-gnu ships first precisely to defer it.**

radlink's PDB/CodeView subsystem is ~20k lines — larger than its entire linking core. Writing
one is a project, not a task. The MinGW path emits DWARF, which requires only section
concatenation plus `SECREL32`/`SECTION` relocation handling.

Phase 5 must choose between (a) write PDB, (b) DWARF-in-PE with LLDB only, (c) ship MSVC ABI
without debug info and document it loudly. **That choice is deferred, not skipped**, and Phase 5
does not start until it is made.

## D6 — macOS ad-hoc code signing. **Locked: mandatory, generate it ourselves.**

Unsigned arm64 Mach-O binaries do not execute on Apple Silicon. Generate the
`LC_CODE_SIGNATURE` blob in-process. Do not shell out to `codesign` — an external tool
dependency in the hot path defeats the entire premise of a fast dev-loop linker.

## D7 — Relocation vocabulary. **Locked: generalize, do not fork.**

wild's core speaks `linker_utils::elf::RelocationKind` (~40 variants), referenced from
`platform.rs:47` and `layout.rs:83`, and consumed by `layout.rs:3013 resolution_flags`. This is
the linker's universal relocation language and PE/COFF must be expressed in it.

Move it to a format-neutral `reld-core` module and **add** the variants COFF needs rather than
creating a parallel enum:

- `ImageRelative` — `IMAGE_REL_AMD64_ADDR32NB`, the RVA-relative form with no ELF analogue.
- `SectionRelative` — `IMAGE_REL_AMD64_SECREL`. **Mandatory**, not optional: MinGW DWARF debug
  sections are full of these.
- `SectionIndex` — `IMAGE_REL_AMD64_SECTION`. Same reason.

Rejected alternative: a parallel `CoffRelocationKind` threaded separately through layout. That
duplicates `resolution_flags` and every GOT/PLT decision site, which is exactly the
fork-the-abstraction mistake that cost LLD 36,441 lines in 2021.

## D8 — Synthetic inputs. **Locked: adopt radlink's synthetic-object architecture.**

Import thunks, the IAT, export tables, and linker-generated symbols are emitted as **synthetic
COFF object files** and fed back through the ordinary input pipeline, rather than being
special-cased as synthetic output sections.

This is the single best architectural idea in radlink (`lnk.c:2135–2248`, `lnk_link_inputs`
called twice at `lnk.c:2123` and `lnk.c:2375`). It collapses a large amount of special-casing:
imports participate in normal symbol resolution, normal COMDAT handling, and normal relocation
processing for free.

## D9 — Symbol table. **Locked: keep wild's `SymbolDb` for v0.**

Do **not** swap in radlink's 4-way CAS hash trie during backend bring-up. Changing the core data
structure while simultaneously adding a format is how you get bugs neither change would have
caused alone.

The trie is recorded as a **later, profiled optimization** (see `09-INCREMENTAL.md` — it becomes
more attractive once a daemon holds the table hot across links). radlink's
`lnk_symbol_hash_trie_insert_or_replace` (`lnk_symbol_table.c:428`) is ~120 lines and its
take-exchange/CAS-put-back merge protocol maps cleanly onto `AtomicPtr::swap` +
`compare_exchange`. Nodes are never freed, which sidesteps reclamation entirely.

## D10 — **REVISED after review 01. The original decision was refuted by measurement.**

The original text said auto-import / pseudo-relocs could simply be rejected, requiring
`__declspec(dllimport)`. **That is false.** Measured on the local MSYS2 UCRT64 gcc 14.2.0
toolchain, a six-line C++ program using `std::string` and `try/throw/catch`:

```
$ g++ -g cpp.cpp -o cpp.exe -Wl,--disable-auto-import,--disable-runtime-pseudo-reloc
ld.exe: cpp.cpp:(.rdata$_ZTISt13runtime_error+0x0): undefined reference to
        `vtable for __cxxabiv1::__si_class_type_info'
```

Those are **data** exports of `libstdc++-6.dll` referenced from a constant initializer. MinGW
C++ headers do not mark them `dllimport` and no compiler flag changes that. "Reject auto-import"
means "no C++ on the default MinGW toolchain."

Revised position for the windows-gnu phase:

- **Auto-import + `--enable-runtime-pseudo-reloc` v2 are REQUIRED**, not optional. ~200 LOC;
  reference `lld/COFF/MinGW.cpp` and mingw-w64's `pseudo-reloc.c`. radlink gives zero help.
- **The default `i386pep` script must be reimplemented, not merely its ordering.** `-T` is still
  rejected (verified: MinGW gcc never passes it), but the script *provides* ~25 symbols the CRT
  undefined-references — `__CTOR_LIST__`, `__RUNTIME_PSEUDO_RELOC_LIST__`, `__rt_psrelocs_*`,
  `__IAT_{start,end}__`, `___crt_x{c,i,l,p,t}_{start,end}__`, `__data_start__`, `etext`, `end` —
  plus a `KEEP` set that doubles as the `--gc-sections` root set.
- **`.rsrc` is required on every link.** Every MinGW executable links `default-manifest.o`, a
  `.rsrc` object. Do not confuse resource *objects* (unconditional) with manifest *files*
  (deferrable).
- **`.CRT$X*` and `.ctors`/`.dtors` both run** on mingw-w64 x86_64. `crt2.o` contains
  `.CRT$XCAA`/`.CRT$XIAA` and references `__xc_a`/`__xc_z`/`__xi_a`/`__xi_z`.
- **`--gc-sections` is passed by default by rustc on windows-gnu**, so it is not an optimization
  and must land before the phase gate.

Still genuinely deferred, each with a clear diagnostic naming the feature:

- **Delay-load imports.**
- **`/GUARD:CF` control-flow guard**, manifests, hotpatch padding.
- **Custom linker scripts** via `-T` — rejected with an explicit message.

## D14 — Wasm. **Locked: delete the backend in P0.**

The fork inherits a **fourth** platform the original plan was entirely unaware of:
`PlatformKind::{Elf, MachO, Wasm}`, with `wasm.rs` (5766) + `wasm_writer.rs` (482) +
`args/wasm.rs` (305) + `wasm_wasm32.rs` (105) ≈ **6,650 LOC** and two dedicated CI jobs
requiring wasmtime, wabt, wasi-libc and wasm-tools.

Delete it in P0 and record the removal in `UPSTREAM.md` as an intentional divergence. Rationale:
consistent with D3's four-target matrix, removes ~6,650 LOC from every future refactor of the
163-member `Platform` trait, and drops two CI jobs.

Accepted cost: raises the upstream-rebase delta, and forecloses wasm as a future target without
re-porting.

**Consequence for every enumeration in the plan:** counts of "add an arm" edit sites were written
assuming three platforms. After deletion they are correct at three (Elf, MachO, Coff); before
deletion they are four. Do the deletion first.

## D15 — Incremental sequencing. **REVISED: the incremental workstream moves ahead of P4/P5.**

The original plan put all incremental work after Phase 4. Combined with `DESIGN.md`'s own
~1.5–2 person-years-per-format estimate, the product feature would not start for 3–4
person-years — by which time, per `09-INCREMENTAL.md`'s own argument, the competitive opening
has closed. That is the wild failure mode exactly: wild was *named* for incremental linking and
has not reached it in two years because the base linker consumed the time.

Revised: the first incremental phase lands **immediately after P2 (Linux proven)**, on ELF
alone, before Windows and macOS.

⚠️ **But its content is not what the plan originally specified.** See review finding R25 — the
phase must be re-derived from measurement before it is scheduled. Caching parsed inputs targets
~5% of link time; string merging is ~66%. This is an open item, not a settled design.

## D11 — Identical Code Folding and `--gc-sections`.

`--gc-sections` is inherited from wild and must keep working on ELF; implement for COFF in
Phase 3 (`/OPT:REF` equivalent). **ICF is deferred indefinitely** — it is a size optimization
(radlink's is ~630 lines of generation-tagged concurrent hash tables) and reld optimizes the
dev loop, where output size is irrelevant.

## D13 — (Thin)LTO. **Locked: stretch goal, sequenced after the fast linker. Not a non-goal.**

LTO is **deferred, not rejected**. The fast linker across Linux, Windows and macOS is the
priority; LTO interacts with nearly every subsystem (symbol resolution, archive extraction,
section layout, debug info) and pulling it forward would slow down the thing the project exists
to deliver. See `DESIGN.md` §3.1.

Consequences for every phase:

- **Do not delete wild's linker-plugin LTO.** The fork inherits a working-with-known-issues
  implementation on ELF (`linker_plugins.rs`, ~1467 LOC, driven from `lib.rs:325` and
  `lib.rs:374`). P2 must not regress it; pin current behaviour with a smoke test.
- **Until LTO is implemented on a given format, its flags are rejected or ignored with a
  specific diagnostic naming the feature** — never silently mislinked. This matters most where
  build systems pass the flag unconditionally: rustc passes `-plugin` on the MinGW gcc path
  (unless `-fno-use-linker-plugin`), and MSBuild passes `/LTCG`.
- When LTO does land it will be **incompatible with the incremental path** — gold and MSVC both
  fall back to a full link for it — so trigger 13 in `09-INCREMENTAL.md` §I1 stays permanent.

Do not treat "LTO is a non-goal" as current. Earlier drafts of `DESIGN.md` and `README.md` said
that; both have been corrected.

## D12 — Test oracle. **Locked: semantic differential, not byte comparison.**

Byte-comparing our output against a reference linker is wrong — reproducible output is an
explicit non-goal and layout differences are legitimate. Adopt wild's methodology:

1. The linker emits a `.layout` sidecar recording which input section landed where
   (`RELD_WRITE_LAYOUT=1`; wild's equivalent is `libwild/src/file_writer.rs:415`).
2. `reld-diff` uses each **input object's relocation list as ground truth**, locates the
   corresponding output bytes in both binaries, infers which relaxation was applied, reverses
   the relocation arithmetic, and compares the recovered **symbol-level target**.

This makes layout differences invisible while catching wrong relocation targets. Every test
written thereafter is automatically also a relocation-correctness test against a reference
linker, for free, forever. Nothing else in either reference project comes close in bug-catching
power per line of test code.
