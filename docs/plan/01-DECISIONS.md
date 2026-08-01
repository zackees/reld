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

## D3 — v0 target matrix. **Locked.**

| Order | Target | Notes |
|---|---|---|
| 1 | `x86_64-unknown-linux-gnu` | Inherited working |
| 2 | `x86_64-pc-windows-gnu` | First new backend |
| 3 | `aarch64-apple-darwin` | Apple Silicon, not x86_64 |
| 4 | `x86_64-pc-windows-msvc` | After COFF is proven |

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

## D10 — MinGW features explicitly NOT supported in v0.

Each must produce a clear diagnostic naming the feature, never a silent mislink:

- **Auto-import / pseudo-relocs** (`_pei386_runtime_relocator`). radlink does not implement these
  either. Error out; require `__declspec(dllimport)`.
- **Delay-load imports.**
- **`/GUARD:CF` control-flow guard**, manifests, hotpatch padding.
- **Linker scripts.** MinGW's `ld` uses default scripts that define section merge order; we
  implement that ordering natively and **reject** `-T` with an explicit message.

## D11 — Identical Code Folding and `--gc-sections`.

`--gc-sections` is inherited from wild and must keep working on ELF; implement for COFF in
Phase 3 (`/OPT:REF` equivalent). **ICF is deferred indefinitely** — it is a size optimization
(radlink's is ~630 lines of generation-tagged concurrent hash tables) and reld optimizes the
dev loop, where output size is irrelevant.

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
