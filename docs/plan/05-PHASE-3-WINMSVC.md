# Phase 3 — PE/COFF core + MSVC ABI (`x86_64-pc-windows-msvc`)

> **RESTRUCTURED after adversarial review.** This was originally "Phase 3 — MinGW." D3 is
> revised to put **msvc first**; MinGW moved to `07-PHASE-5-WINGNU.md`. Debug info is deferred
> per D5 — **accepted cost: no debugger on Windows until D5 is resolved.**

The largest phase. It delivers the **COFF core shared by both Windows ABIs**, plus the MSVC
argument dialect and CRT conventions.

Why msvc first: radlink is a 19.7k-LOC in-tree reference targeting exactly this, carrying
COMDAT, weak externals, base relocations, `.rsrc`, TLS, imports/exports and `.drectve`
case-for-case. Every MinGW-specific requirement the review surfaced — auto-import,
default-linker-script emulation, `.CRT$X*`-plus-`.ctors` — falls in the region where radlink
gives **zero** help. Doing msvc first drives the net-new-without-reference surface to near zero.

Reference: `.extern-repos/raddebugger/src/linker/` (radlink, MIT). Its **algorithms** may be
studied and reimplemented freely. One limit: its relocation patcher is **x64-only**
(`lnk.c:4510` is `lnk_not_implemented` for other machines).

## Task split between the two Windows phases

**Stays here (COFF core + MSVC):** T1 relocation vocabulary · T2 file-kind sniffing · T4
`ObjectFile` · T5 `Platform` impl · T6 COMDAT · T7 weak externals · T8 grouped-section `$`
ordering · T9 PE headers · T10 relocation application · T11 base relocations · T12
`.pdata`/`.xdata` · T13 imports/IAT · T14 exports/DLL · T16 TLS · T18 `--gc-sections` ·
`coff_writer.rs` · COFF `linker-diff` · `.rsrc` · `.drectve` · `link.exe` dialect · MSVC CRT and
`.CRT$X*` ordering.

**Moves to `07-PHASE-5-WINGNU.md`:** GNU arg dialect and the `Args::new()` pre-scan · auto-import
and runtime pseudo-relocs · default `i386pep` script emulation · `.ctors`/`.dtors` · MinGW entry
points · DWARF-in-COFF (T15) · long-form `.idata$N` import libraries.

## Tasks added by review — none were in the original plan

- **`coff_writer.rs`** (R7). The output-writing module appears in no task, yet by precedent it is
  1,334–6,154 LOC. T9/T11/T13/T14 describe its contents without registering the module.
- **COFF `linker-diff` instantiation** (R8, R30). ~1,500–2,500 new LOC, and a phase-exit gate per
  D17. Note `object` reports `implicit_addend: true` for COFF, with `addend` holding only the
  fixed PC bias; `asm_diff` treats `rel.addend()` as the complete symbolic offset at six sites,
  so a naive port makes **every recovered referent wrong**.
- **`IMAGE_SCN_LNK_NRELOC_OVFL`** (R4) — belongs in T4. Measured: plain `gcc -c` on a
  70,000-pointer array emits it. Reading `NumberOfRelocations` literally applies 65,535 of
  70,001 relocations and yields a binary that links, runs, and is **wrong**. radlink has zero
  handling, and the oracle will not catch it if it only samples relocations the reader found.
- **Section alignment from `IMAGE_SCN_ALIGN_*` characteristics** — ELF carries alignment in a
  dedicated field; COFF encodes it in section flags. Exactly the gap an ELF port produces.
- **`object` crate features** — the root `Cargo.toml` enables `elf` and `macho` but **not `coff`
  or `pe`**, which D4's premise requires.
- **`input_data.rs`** (R7) — 15 format-specific references covering archive-member handling and
  per-kind routing, where import libraries land. Named in no task.
- **`Architecture` handling** (R22) — `platform.rs:1567` defaults `architecture()` to
  `Unsupported` and `layout.rs:5884` silently coerces it to `X86_64`. A COFF platform that
  forgets to override is silently treated as ELF x86-64 for layout rather than erroring.
- **Archive member ordering / repeated groups** — link lines list library groups twice to resolve
  cycles; a single-pass resolver fails on them.
- **`PlatformKind::host()` → `Coff` on Windows** moves here from P0 (R13).

## Ordering fix (R15)

Group A/B acceptance criteria required binaries that *link and run*, but the writer, relocation
application, imports and entry point are all Group C — so T6–T12 were blocked under global rule
1. **Restate T6–T8's acceptance against the `.layout` sidecar and symbol-resolution assertions;
move every "links and runs" criterion to the phase gate.**

## Correction (R9)

T1 was called "the single most invasive change in this phase." It is not — wild **already**
reuses the ELF relocation vocabulary unchanged across Mach-O and Wasm, and `RelocationKindInfo`
carries no ELF-only fields. Realistically **4–8 files, 150–300 LOC, 1–3 days**. Two caveats
survive: `RelocationKind::PairSubtractionULEB128` carries a raw ELF `r_type` payload, and adding
COFF variants breaks exhaustive matches at `layout.rs:3013`, `elf_writer.rs:3061` and `:3481`.

Also measured: `IMAGE_REL_AMD64_SECTION` was **never observed** in GCC output across `crt2.o`,
`g++ -g` output and all 189 members of `libstdc++.a` — D7's "`SectionIndex` is mandatory" is
overstated. Implement it (it is trivial), but it is not load-bearing. Census: `ADDR32NB` 9,715 ·
`REL32` 18,903 · `ADDR64` 3,061 · `SECREL` 532.

---

## Group A — Core plumbing

### P3-T1 — Generalize the relocation vocabulary (D7)

The single most invasive change in this phase. Do it first, alone, on ELF only, and prove ELF
still passes before any COFF code exists.

1. Move `RelocationKind`, `RelocationKindInfo`, `DynamicRelocationKind` out of
   `crates/reld-reloc/src/elf.rs` into a format-neutral `crates/reld-reloc/src/kind.rs`.
2. Update the references in `crates/reld-core/src/platform.rs:47-48` and
   `crates/reld-core/src/layout.rs:83`.
3. Add three variants required by COFF and **not** expressible in ELF's set:
   - `ImageRelative` — `IMAGE_REL_AMD64_ADDR32NB` (RVA-relative)
   - `SectionRelative` — `IMAGE_REL_AMD64_SECREL`
   - `SectionIndex` — `IMAGE_REL_AMD64_SECTION`

`SectionRelative` and `SectionIndex` are **mandatory, not optional**: MinGW DWARF debug sections
are full of them. Skipping them means no debug info, which forfeits the entire reason win-gnu
goes first.

4. `layout.rs:3013 resolution_flags` must handle the new variants (they need neither GOT nor
   PLT).

`Acceptance:` `cargo test --test acceptance -- elf/` fully green, with zero behavioural change.

### P3-T2 — File-kind sniffing

`crates/reld-core/src/file_kind.rs:17` — add `CoffObject`, `PeImportLibrary`. `identify_bytes`
(line 33) sniffs the COFF machine header (`0x8664`) and the `MZ`/`PE\0\0` pair. Handle
`/bigobj` (the alternate GUID-prefixed header with 32-bit symbol indices) — MinGW C++ with heavy
templates does emit it.

Also update `Platform::is_allowed_in_archive` for the new kinds.

`Acceptance:` unit tests over fixture bytes for each kind.

### P3-T3 — `CoffArgs` and the GNU-dialect-with-COFF-platform combination

Create `crates/reld-core/src/args/coff.rs`, modelled on `args/macho.rs` (426 LOC). Add
`Args::Coff` and update the five `match self` sites in `args.rs` (lines 140, 171, 192, 200, 209).

**The dialect and the format are independent axes** (D2). MinGW drives a GNU-syntax `ld`. So the
COFF platform must be reachable from the *GNU* parser via `-m i386pep`, and the existing
`args/elf.rs` declarations for `-L -l -o -e --gc-sections --whole-archive` must be shared, not
duplicated. Extract them into `declare_common_args` (`args.rs:1341`) rather than copy-pasting.

MinGW-specific flags to accept: `--out-implib`, `--subsystem`, `--image-base`,
`--enable-auto-image-base`, `--major/minor-os-version`, `--dll`, `--export-all-symbols`.

Per D10, `-T` (linker script) must be **rejected with an explicit message**, not ignored.

`Acceptance:` `ld.reld -m i386pep --version` selects the COFF platform; `reld -flavor gnu -m
i386pep -T foo.ld` errors naming linker scripts as unsupported.

### P3-T4 — `ObjectFile` impl for COFF

Implement the 37-method `ObjectFile<'data>` trait (`platform.rs:956`) over `object`'s COFF
reader (D4). Section enumeration, symbol enumeration, relocations, section data.

COFF specifics that differ from ELF and will bite:
- **Addends are stored in-place in the section bytes**, REL-style, not in the relocation record.
  Read, sign-extend, add.
- Symbol storage classes (`IMAGE_SYM_CLASS_*`) map onto binding/visibility differently from
  ELF's `STB_`/`STV_`.
- Section numbers are 1-based; `IMAGE_SYM_UNDEFINED`(0), `ABSOLUTE`(-1), `DEBUG`(-2) are
  sentinels.
- Auxiliary symbol records carry the COMDAT selection and section definition.

**No `todo!()`** (global rule 3). Any unhandled construct `bail!`s naming the object file.

`Acceptance:` a fixture test parses a `gcc -c` MinGW object and enumerates its sections and
symbols correctly.

### P3-T5 — `Platform` impl skeleton

`crates/reld-core/src/coff.rs` — `struct Coff` plus the `Platform` impl and its supporting trait
impls (`SectionHeader`, `SectionType`, `SectionFlags`, `Symbol`, `SectionAttributes`,
`ProgramSegmentDef`, `Relocation`, `RelocationList`, `RawSymbolName`, `BuiltInSectionDetails`).

For the ELF-only associated types with no COFF meaning — `VerneedTable`, `VersionNames`,
`SymbolVersionIndex`, `DynamicTagValues`, symbol versioning generally — use unit types, exactly
as the Mach-O backend does.

Register: `lib.rs` module declarations, the `lib.rs:267` dispatch arm, and the feature gate
`coff = []` in `Cargo.toml`. **Unlike wild's `macho` feature, `coff` is on by default** — a
feature-gated-off backend that panics at runtime is the failure mode we are avoiding.

`Acceptance:` `cargo build --workspace` with the COFF platform selectable; linking still
`bail!`s cleanly.

---

## Group B — Semantics

### P3-T6 — COMDAT selection

The classic silent-wrong-answer generator, and unavoidable — it is COFF's core mechanism for C++
inline functions and templates.

Port radlink's `lnk_can_replace_symbol` (`lnk_symbol_table.c:152–384`) **case for case**. It is
effectively a specification of COFF symbol resolution. All six selection kinds:

| Selection | Behaviour |
|---|---|
| `NODUPLICATES`(1) | duplicate ⇒ error |
| `ANY`(2) | first by deterministic input order wins |
| `SAME_SIZE`(3) | equal size ⇒ input order; else error |
| `EXACT_MATCH`(4) | checksum then byte compare ⇒ input order; else error |
| `ASSOCIATIVE`(5) | no independent choice; follows its associated section |
| `LARGEST`(6) | bigger wins; tie by input order |

Two details that are easy to miss and expensive to get wrong:
- A **common symbol is modelled as a COMDAT with `select = LARGEST` and `section_length = value`**.
- If one side is `ANY` and the other `LARGEST`, **both are promoted to `LARGEST`**.

Losing a COMDAT election must mark the loser's section *and every section associated with it*
(`.pdata`/`.xdata` attach via `ASSOCIATIVE`) as removed — radlink's `lnk_on_symbol_replace`
(`lnk_symbol_table.c:387`).

Determinism comes entirely from the `(archive index, object index, symbol index)` tiebreak
(global rule 4).

`Acceptance:` fixtures for all six selections including the error cases; a C++ fixture with an
inline function in three translation units links and runs.

### P3-T7 — Weak externals

COFF weak externals are **not** ELF weak symbols. Each carries a `Characteristics` field:
`SEARCH_NOLIBRARY`(1), `SEARCH_LIBRARY`(2), `SEARCH_ALIAS`(3), and the anti-dependency case.
Port radlink's handling (`lnk_symbol_table.c:185–270`), including its deliberate,
documented divergence from MSVC where it matches LLD.

Weak→tag chains must be cycle-detected and reported with the full chain (radlink's
`lnk_resolve_weak_symbol`, `lnk_symbol_table.c:683`), not left to recurse.

`Acceptance:` fixtures for each characteristic value plus a deliberate weak cycle producing a
clear error.

### P3-T8 — Output sections and grouped-section ordering

COFF's `$` convention: `.text$mn` contributes to `.text`, and the substring after `$` is the
**sort key within the output section**, then discarded. MinGW relies on this for
`.ctors`/`.dtors` ordering.

Default mapping: `.text$*`→`.text`, `.rdata$*`→`.rdata`, `.data$*`→`.data`, `.bss`,
`.pdata`, `.xdata`, `.tls$*`→`.tls`, `.idata$*`→`.idata`, `.eh_frame`, `.debug_*` (non-alloc),
`.ctors`/`.dtors` (MinGW, ordered).

`Acceptance:` a fixture with `.text$a`/`.text$c`/`.text$b` produces contributions in `a,b,c`
order.

---

## Group C — Output

### P3-T9 — PE headers

`lnk_build_win32_header` (`lnk.c:5381`) is ~230 lines of straight serialization; ours will be
similar. DOS stub, `PE\0\0`, COFF header, optional header (PE32+), section table, 16 data
directories.

Fixed choices for v0: `SectionAlignment` 0x1000, `FileAlignment` 0x200, `ImageBase`
0x140000000 (exe) / 0x180000000 (dll), `DllCharacteristics` = `DYNAMIC_BASE | NX_COMPAT |
HIGH_ENTROPY_VA`. Note `DYNAMIC_BASE` **requires** `.reloc` (P3-T11).

`Acceptance:` output passes `llvm-readobj --file-headers` without error and matches a reference
linker's header fields modulo the documented ignore list.

### P3-T10 — Relocation application, x86_64

`IMAGE_REL_AMD64_`: `ABSOLUTE`(0, no-op), `ADDR64`(1), `ADDR32`(2), `ADDR32NB`(3),
`REL32`(4), `REL32_1..5`(5–9), `SECTION`(10), `SECREL`(11).

`ADDR32NB` is image-relative (target RVA − image base), and MinGW's `__ImageBase` symbol must
resolve to the image base. `SECTION`/`SECREL` are needed by DWARF (P3-T15).

Follow mold's structural insight (`design.md:388-397`): apply relocations **during the copy into
the output buffer**, not as a later pass — the section contents are already in cache, making it
close to free. Both radlink (`lnk_obj_reloc_patcher`, `lnk.c:4470`) and mold do this, and
radlink parallelizes it per-object since every target address is final and writes are disjoint.

`Acceptance:` one fixture per relocation kind, verified by the P1-T3b differential oracle
against `ld.lld`.

### P3-T11 — Base relocations (`.reloc`)

Port radlink's shape (`lnk_build_base_relocs`, `lnk.c:5287`, ~250 lines): gather
`HIGHLOW`/`DIR64` entries per 4 KiB page in parallel → merge per-worker page tables → dedupe →
sort → serialize. Well-factored and worth copying nearly wholesale.

`Acceptance:` a fixture linked with `DYNAMIC_BASE` runs correctly when relocated; `.reloc`
contents match a reference linker's set.

### P3-T12 — `.pdata` / `.xdata`

Pleasantly small. mingw-w64 x86_64 defaults to SEH, so the compiler emits unwind data and the
linker only **concatenates `.xdata` and sorts `.pdata` by function start RVA**, then points the
Exception data directory at it. radlink does no unwind-data construction at all.

`Acceptance:` a fixture that throws and catches a C++ exception across a translation-unit
boundary returns 42.

### P3-T13 — Archives, import libraries, and the IAT

The largest single item in Group C. Per D8, implement via **synthetic COFF objects** fed back
through the normal input pipeline.

- MinGW `dlltool` import libraries contain both **short-import members** (`COFF_DataType_Import`)
  and ordinary objects with `.idata$N` sections. Handle both: the `.idata$N` form falls out of
  grouped-section merging for free (P3-T8).
- For each imported symbol, synthesize the import thunk, ILT and IAT entries, and the
  `__imp_<name>` symbol. x86_64 jump thunk is a hand-assembled `ff 25 <rel32>`.
- Populate the Import data directory.

Per D10, **auto-import / pseudo-relocs are not supported** — a reference to a DLL symbol without
`__declspec(dllimport)` must produce a clear diagnostic, never a silent mislink. radlink does
not implement these either.

`Acceptance:` a fixture calling `MessageBoxA` from `user32` links and runs; a fixture relying on
auto-import fails with a message naming the feature.

### P3-T14 — Exports, DLLs, `--out-implib`

Export directory construction, `.edata`, ordinal assignment, `--export-all-symbols`, `.def` file
parsing, and generation of the companion import library. Reference:
`pe/pe_make_export_table.c` (339 LOC).

`Acceptance:` build a DLL plus its import lib with reld, link an exe against it with reld, run.

### P3-T15 — DWARF debug sections

The payoff for choosing gnu first. `.debug_*` sections are non-alloc: concatenate and relocate
them like any other section. The only special requirement is correct `SECREL`/`SECTION`
relocation handling from P3-T1 and P3-T10.

`Acceptance:` `gdb` (MSYS2) sets a breakpoint by file:line in a reld-linked binary and hits it.
This is a scripted acceptance test, not a manual check — drive gdb in batch mode.

### P3-T16 — TLS directory

~35 lines in radlink (`lnk.c:6073–6107`): locate `_tls_used`, scan `.tls` contributions for
maximum alignment, OR the alignment bits into the TLS header characteristics, set the data
directory.

`Acceptance:` a fixture using `__thread` returns 42.

### P3-T17 — Entry point, subsystem, CRT integration

MinGW entry points: `mainCRTStartup` (console), `WinMainCRTStartup` (GUI), `DllMainCRTStartup`
(DLL). Subsystem inference from which of `main`/`WinMain` is defined. `.ctors`/`.dtors` list
construction with `__CTOR_LIST__`/`__DTOR_LIST__` sentinels — this is GNU-specific and has no
radlink analogue (MSVC uses `.CRT$XC*`).

`Acceptance:` a fixture with a C++ global constructor observes the constructor having run.

### P3-T18 — `--gc-sections` for COFF

The `/OPT:REF` equivalent. wild's `find_required_sections` (`layout.rs:2303`) is already
format-generic; the work is supplying COFF's root set and making COMDAT-associative sections
follow their leader.

`Acceptance:` a fixture with an unreferenced function confirms the function is absent.

---

## Group D — Gate

### P3-T19 — Phase gate

All must pass on the `windows-latest` MSYS2 UCRT64 CI job:

1. Hello-world C links and runs, exit code 42.
2. `cargo build --target x86_64-pc-windows-gnu` of a nontrivial crate, linked by reld via
   `-Clink-arg=-fuse-ld=<path>`, produces a running binary.
3. gdb hits a breakpoint by file:line (P3-T15).
4. C++ exception across translation units (P3-T12).
5. DLL round-trip (P3-T14).
6. Differential oracle green against `ld.lld` across the whole COFF fixture set.
7. `reld-difftest --seeds 500` green on windows-gnu.

`Acceptance:` `ci/gate-wingnu.sh` exits 0.
