# Adversarial review 01 — findings against the initial plan

Six reviewers attacked the plan from different angles: internal consistency, wild-fork
feasibility (source-verified), PE/COFF + MinGW (toolchain-measured), Mach-O, incremental
soundness, and the test oracle.

**Status: the plan does not survive contact unamended.** Two locked decisions are refuted by
direct measurement, one acceptance criterion is architecturally impossible, and the plan is
unaware of a fourth backend it would inherit.

---

## Refuted decisions

### R1 — D10 "auto-import / pseudo-relocs can simply be rejected" is FALSE ⚠️ CRITICAL

Measured on the local MSYS2 UCRT64 gcc 14.2.0 toolchain. A six-line C++ program using
`std::string` and `try/throw/catch`, built with plain `g++`:

```
$ g++ -g cpp.cpp -o cpp.exe -Wl,--disable-auto-import,--disable-runtime-pseudo-reloc
ld.exe: cpp.cpp:(.rdata$_ZTISt13runtime_error+0x0): undefined reference to
        `vtable for __cxxabiv1::__si_class_type_info'
collect2.exe: error: ld returned 1 exit status
```

The successfully default-linked binary contains a non-empty pseudo-reloc list
(`__rt_psrelocs_size = 0x24`). The failing symbols are **data** exports of `libstdc++-6.dll`
referenced from a constant initializer — exactly the case LLD documents as requiring runtime
pseudo-relocs. MinGW C++ headers do not mark these `dllimport` and no compiler flag fixes it.

**D10 as written means "no C++ on the default MSYS2 toolchain."** Phase 3's own gate item 4
(C++ exception across translation units) cannot pass.

**Fix:** D10 amended — auto-import + `--enable-runtime-pseudo-reloc` v2 become a required P3
task (~200 LOC, reference `lld/COFF/MinGW.cpp` and mingw-w64's `pseudo-reloc.c`). radlink gives
zero help here.

### R2 — "`-m i386pep` selects the COFF platform" is architecturally impossible ⚠️ CRITICAL

D2 and P3-T3 specify selecting the COFF platform via the MinGW emulation flag. wild chooses the
`Args` enum variant in `Args::new()` (`args.rs:113-149`) from **only** argv[0], a literal
`-flavor` at argv[1], or the host — *before any parsing occurs*. `-m` is a sub-option handler
inside the ELF parser (`args/elf.rs:527-568`) whose closures take `&mut ElfArgs` and cannot
change which variant exists.

**Fix:** either pre-scan argv before `Args::new()`, or have `CoffArgs` host the whole GNU
dialect. Either way the shared-dialect refactor (making the parser combinators generic over the
args struct) is unbudgeted work that P3-T3 currently assumes is free.

---

## Unknown-unknowns the plan missed

### R3 — wild has a **fourth** backend: Wasm ⚠️ CRITICAL

The string "wasm" appears **zero times** across all ten plan documents. Reality:
`args.rs:225` is `enum PlatformKind { Elf, MachO, Wasm }`; `lib.rs:267` is a three-arm match;
`wasm.rs` (5766) + `wasm_writer.rs` (482) + `args/wasm.rs` (305) + `wasm_wasm32.rs` (105) ≈
6,650 LOC inherited, with two dedicated CI jobs requiring wasmtime, wabt, wasi-libc and
wasm-tools.

Every "add an arm" instruction in the plan is off by one, and P1-T1's frozen
`Platform:{elf|coff|macho}` directive cannot express inherited wasm fixtures — which, with a
reject-unknown parser, is a hard failure.

**Fix:** a P0 task that explicitly either deletes the Wasm platform (recording it in
`UPSTREAM.md` as an intentional divergence) or budgets it. Deletion is cheaper and consistent
with D3's four-target matrix.

### R4 — `IMAGE_SCN_LNK_NRELOC_OVFL` → silent mislink ⚠️ CRITICAL

Measured: an array of 70,000 pointers compiled with plain `gcc -c` produces
`.data nreloc=65535 flags=c1600040` with the overflow bit set; the real count lives in
`reloc[0].VirtualAddress` and the first record must be skipped. radlink has **zero** handling
(`grep -ric OVFL src/linker/*.c` → 0).

Reading `NumberOfRelocations` literally applies 65,535 of 70,001 relocations and produces a
binary that links, runs, and is **wrong**. Rust and C++ with large generated tables hit this.

**Fix:** explicit requirement in P3-T4, and a fixture. Note the differential oracle will *not*
catch this if the oracle only samples relocations the reader already found.

### R5 — MinGW's default linker script defines ~25 symbols the CRT references

`ld -m i386pep --verbose` is not only ordering — it *provides* `__CTOR_LIST__`, `__DTOR_LIST__`,
`__RUNTIME_PSEUDO_RELOC_LIST__(_END__)`, `__rt_psrelocs_{start,end,size}`, `__data_start__`,
`__bss_start__`, `__IAT_{start,end}__`, `___crt_x{c,i,l,p,t}_{start,end}__`, `___tls_{start,end}__`,
`etext`, `end`, and more. Verified consumed: `nm libmingw32.a` shows undefined references to
them.

P3-T17 mentions three. The other ~20 are each an undefined-reference hard failure on hello-world.

**Fix:** P3-T17 becomes "reimplement the `i386pep` default script" — ordering, the full provided-
symbol set, and `KEEP` semantics.

### R6 — Missing COFF tasks, each hit on every MinGW link

- **`.rsrc`** — every MinGW executable links `default-manifest.o`, a `.rsrc` object. The plan's
  section map omits `.rsrc` entirely, and D10 conflates *manifest files* (deferrable) with
  *resource objects* (unconditional). Multiple `.rsrc` trees need directory-tree merge, not
  concatenation.
- **`.CRT$X*`** — live on mingw-w64 x86_64. `crt2.o` contains `.CRT$XCAA`/`.CRT$XIAA` and
  references `__xc_a`/`__xc_z`/`__xi_a`/`__xi_z`. The plan's framing (".ctors/.dtors is GNU,
  .CRT$XC* is MSVC") is wrong — **both** run.
- **`--gc-sections` is passed by default by rustc on windows-gnu**, so P3-T18 is not an
  optimization and must move earlier. Its root set must include the script's `KEEP` list
  (`.idata$2..$7`, `.CRT$X*`, `.ctors*`, `.rsrc*`, `.rdata_runtime_pseudo_reloc`) or gc silently
  deletes import descriptors.
- **DWARF sections are COMDAT-grouped in MinGW** (`.debug_frame$_ZN1GC1Ev`, `LINK_ONCE_DISCARD`).
  P3-T15's "concatenate and relocate like any other section" is wrong; `.debug_*` participates
  in both `$`-grouping and COMDAT election, and a losing `.debug_frame$X` must be discarded with
  its `.text$X`. Failure mode is not a link error — it is gdb reading stale CFI.
- **Import libraries are the long `.idata$N` dlltool form**, not short-import members
  (`libuser32.a` has 983 members: head/tail/stubs). P3-T13's "falls out of grouped-section
  merging for free" is false — the script's `KEEP(SORT(*)(.idata$2))`…`$7` order sorts by
  *filename*, not by `$` suffix, and null-terminator objects must sort last.
- **Section alignment comes from `IMAGE_SCN_ALIGN_*` in section characteristics**, not a
  dedicated field as in ELF. Never mentioned — exactly the gap an ELF port produces.
- **Archive member ordering / repeated groups** — gcc lists `-lmingw32 -lgcc -lgcc_eh -lmingwex
  -lmsvcrt -lkernel32` *twice* to resolve cycles. A single-pass archive resolver fails on it.

### R7 — Registration surface larger than P3-T5's list

Source-verified against what Mach-O actually had to touch: `input_data.rs` has **15**
format-specific references (`:521-819`) covering archive member handling and per-kind routing —
where COFF import libraries land — and is never mentioned. Also missing: `file_kind.rs:140`
(`Display`), `part_id.rs:180` (compile-time per-platform assertion), `alignment.rs:66`,
`grouping.rs:8`, `resolution.rs:23`.

**No `coff_writer.rs` appears in any task.** By precedent it is 1,334–6,154 LOC. P3-T9/T11/T13/T14
describe its contents without registering the module.

The root `Cargo.toml` `object` features are `["compression","elf","macho",...]` — **no `coff`,
no `pe`**, which D4's entire premise requires.

### R8 — `linker-diff` is not COFF/Mach-O aware, and no task makes it so

D12 and five acceptance criteria depend on the differential oracle across formats. `linker-diff`
is 10,195 LOC with `object::elf` in 18 of its files and `asm_diff.rs` carrying 126
`RelocationKind::` references. P1-T3b says "the COFF instantiation lands in Phase 3" — Phase 3
has 19 tasks and none of them is that work. The Mach-O instantiation is not promised anywhere.

Likewise P1-T2 extends only the layout *schema*, not the per-format writers.

---

## Estimation errors

### R9 — P3-T1 (relocation generalization) is overstated; everything else in P3 is understated

The plan calls it "the single most invasive change in this phase." It is not: wild **already**
reuses the ELF relocation vocabulary across all three backends unchanged
(`macho_aarch64.rs:11-14`, `wasm_wasm32.rs:16`), and `RelocationKindInfo` carries no ELF-only
fields. The move is a file relocation plus a `pub use` — **4–8 files, 150–300 LOC, 1–3 days.**

Two real caveats survive: `RelocationKind::PairSubtractionULEB128` carries a raw ELF `r_type`
payload, and adding COFF variants breaks exhaustive matches at `layout.rs:3013`,
`elf_writer.rs:3061` and `:3481`.

Also measured: `IMAGE_REL_AMD64_SECTION` was **never observed** in GCC output across `crt2.o`,
`g++ -g` output, and all 189 members of `libstdc++.a` — GCC's DWARF uses `secrel32` only. D7's
"`SectionIndex` is mandatory" is overstated (implement it; it is trivial; it is not
load-bearing). Census: `ADDR32NB` 9,715 · `REL32` 18,903 · `ADDR64` 3,061 · `SECREL` 532.

### R10 — The `Arch` axis is cheap; the Platform+writer axis is not

`macho_aarch64.rs` is 223 lines for a whole format+arch combination because per-arch relocation
encoding lives in the format-independent `linker-utils/{x86_64,aarch64}.rs`. So `coff_x86_64.rs`
is ~200–400 LOC. The real cost is Platform + writer: Mach-O totals ~4,300 LOC *for an admittedly
incomplete backend*. COFF with COMDAT, weak externals, imports/exports, `.reloc`, `.pdata`,
auto-import and DWARF-in-PE will be larger.

### R11 — Feature-gate reasoning is based on a misreading

P3-T5 says "unlike wild's `macho` feature, `coff` is on by default — a feature-gated-off backend
that panics at runtime is the failure mode we are avoiding." wild's `macho` feature does **not**
cfg-gate the module out and does **not** panic: modules are unconditional (`lib.rs:40-44`) and
`macho.rs:73-77` is a runtime `bail!` on `cfg!(feature = "macho")`.

Copying that pattern gets what the plan wants with no upstream divergence. Also, a bare
`coff = []` entry is *off* by default anyway. And because modules always compile while CI runs
`--no-default-features` with `-D warnings`, an always-on incomplete backend must be
warning-clean under both configurations from the first commit — work-in-progress cannot hide
behind `#[cfg]`. wild has a `wip = []` feature that is the idiomatic home.

---

## Process and consistency defects

### R12 — P0's acceptance is unachievable, three ways

1. **Vendoring is incomplete.** P0-T1 copies six crate directories but not `fakes/` (the
   symlink dir the `-B` mechanism depends on), the root `ld` symlink, `test-config-ci.toml`,
   `deny.toml` (which `cargo deny check licenses` needs), `rustfmt.toml`, `docker/`, or
   `external_test_suites/`.
2. **Edition/MSRV collision.** Vendored wild is `edition = "2024"`, `rust-version = "1.94"`; the
   reld workspace is `edition = "2021"` with an unpinned stable toolchain.
3. **Wild never runs its test suite on Windows, and not on bare `ubuntu-latest` either.** Its CI
   uses SHA-pinned prebuilt containers with gcc/g++/binutils for four cross triples, qemu-user,
   nightly with cranelift, and `WILD_TEST_CONFIG=test-config-ci.toml`. The tests shell out to
   `gcc`, `clang`, `ld.lld`, `ar`, `objcopy`, `bash`, `getconf`, `qemu-*`, `wasm-tools`,
   `wasmtime`. Exactly one `#[cfg(unix)]` gate exists in 7,092 lines, so on Windows they compile
   and fail at runtime rather than skipping.

**This matters beyond P0:** development happens on Windows 10. The plan needs an explicit
Linux dev-container story (vendor wild's `docker/`) and must state that P0–P2 acceptance is
container-only.

### R13 — P0-T5 bricks the dev machine for three phases

Setting `PlatformKind::host()` → `Coff` on Windows means a bare `reld` on the dev box, and every
Windows CI job, hits the Phase-3 `bail!`. Defer the `host()` change to P3-T5.

### R14 — `-fuse-ld=` does not work with MinGW gcc; three phase docs use it anyway

Measured: `gcc -fuse-ld=/abs/path/to/ld` → `unrecognized command-line option`. GCC accepts only
`bfd|gold|lld|mold`; absolute paths are a clang extension. `08-ACCEPTANCE.md` already mandates
the `-B<dir>` form *and says why*; P2-T2, P3-T19.2 and P4-T10.2 contradict it.

Also measured — rustc's real windows-gnu link line passes `-l:libpthread.a` (literal-filename
form), `-Bstatic`/`-Bdynamic`, `--nxcompat`, `--dynamicbase`, `--high-entropy-va`,
`--disable-auto-image-base`, `--gc-sections`, `-no-pie`, and invokes `x86_64-w64-mingw32-gcc`
rather than `gcc`. Plain `gcc`/CMake additionally pass 18 `-plugin-opt=` arguments (rustc dodges
this with `-fno-use-linker-plugin`). None of these are in P3-T3's flag list; the link aborts on
argument one.

### R15 — Phase-internal ordering violations

P3 Group A/B acceptance criteria require binaries that *link and run* (T6 `:138`, T11 `:206`,
T12 `:216`) but the PE writer, relocation application, imports and entry point are all Group C.
Under global rule 1 (one task, one acceptance command) tasks T6–T12 are blocked.

Restate T6–T8 against the `.layout` sidecar and symbol-resolution assertions; move all
"links and runs" criteria to T17 or the T19 gate.

### R16 — Cross-phase dependency violations

- **P1-T8** ("all four CI jobs green") requires COFF and Mach-O backends that do not exist until
  P3/P4 — and the same task mandates `RELD_VERIFY_PLATFORM_REQUIREMENTS=1`, which converts
  "everything skipped" from green to red. P1 can never close as written.
- **P2-T1** requires aarch64 ELF tests green; wild runs those under **qemu**, which P1-T8 and
  `08-ACCEPTANCE.md` §5 explicitly ban.
- **P1-T1** freezes the directive set at 18 with a reject-unknown parser; **P2-T1** then feeds
  it 213 fixture directories authored against wild's ~90. The translation is the largest
  unbudgeted item in P1/P2 and is invisible.

### R17 — Orphans: referenced but created by no task

`ld.reld` / `reld-link` / `ld64.reld` (required by four acceptance commands; P0 produces one
binary named `reld`, and wild's mechanism is symlinks, which do not survive a Windows checkout),
`ci/gate-linux.sh`, `ci/gate-incremental.sh`, `deny.toml`.

### R18 — P0 breaks all four existing workflows

`ci.yml:39-49` runs `cargo run --bin reld -- --targets` and asserts the binary **exits non-zero
by design** — a contract P0 deliberately invalidates. `ci.yml:31-34` runs `cargo fmt --check`
and `clippy -D warnings` over ~30k LOC of vendored upstream code. `stress.yml` and
`sanitizers.yml` trigger on `paths: crates/**`, so every vendored-code PR fires them.

### R19 — Documentation left factually false

`DESIGN.md:3-5` still says "**No implementation exists yet**" and §1 still says "successor to"
wild — the exact phrasing P0-T2 changes in the README *because the distinction is legally
load-bearing*. No task updates DESIGN.md. Also `00-OVERVIEW.md:79` claims wild has "not even a
design sketch" for incremental, which `09-INCREMENTAL.md:61` refutes (published Nov 2024).

### R20 — Nine acceptance lines are not shell commands

Direct violation of global rule 2. Worst is P0-T6's "manual review." Each needs a named test
filter, e.g. `cargo test --test acceptance -- coff/comdat`.

### R21 — `WILD_UNSUPPORTED` defaults to `"warn"`

`platform.rs:1558`. Unsupported options are silently warned-and-continued, directly contradicting
the plan's "never a silent mislink" rule. Flipping it to `"error"` will surface latent ELF gaps
during P2 — schedule it there, not later.

### R22 — Silent-wrong-answer path in `Architecture`

`platform.rs:1567` defaults `architecture()` to `Unsupported`; `layout.rs:5884-5885` then
silently coerces `Unsupported → X86_64`. A COFF platform that forgets to override is treated as
ELF x86-64 for alignment and layout rather than erroring. `args/elf.rs:2040` turns the same
value into an `unreachable!()` panic. `arch.rs` is 69 lines and every one is ELF-specific
(`parse_output_format` only accepts `elf64-*`).

### R23 — D5's option (b) is mispriced for MSVC

D5 prices "DWARF-in-PE" as "small — Phase 3 already does it." Phase 3's DWARF works because
*mingw gcc emits DWARF*. `cl.exe` emits CodeView only and has no DWARF mode. P5-T4's gate item 1
is a `cl.exe`-built hello-world, so the debug test cannot exist under option (b) without
clang-cl.

### R24 — The schedule falsifies the strategy, and the plan never says so

`DESIGN.md` states ~1.5–2 person-years per format, "three formats is three times one plus the
abstraction tax." `09-INCREMENTAL.md` argues the competitive opening is time-limited. Taken
together, the product feature does not start for 3–4 person-years, by which time the opening has
closed by the plan's own argument.

**The single highest-leverage change available and never considered: I2 (daemon + warm full
link, zero patching) is buildable on ELF alone and is explicitly described as "the first
shippable win." Reordering it ahead of P4/P5 de-risks the entire project.**

---

## The strategic question this review raises

The COFF reviewer's corrected cost ledger:

| | windows-gnu | windows-msvc |
|---|---|---|
| Debug info | DWARF: concat + SECREL + **COMDAT'd `.debug_frame`** | PDB (~20k LOC) — or defer per D5 |
| Auto-import + pseudo-reloc v2 | **required** (R1) | n/a |
| Default linker-script emulation (~25 symbols, KEEP set, `.idata$N` order) | **required** (R5, R6) | n/a — `link.exe` semantics *are* the spec |
| `.rsrc` | **required on every link** | required |
| `.CRT$X*` + `.ctors`/`.dtors` | **both** | `.CRT$X*` only |
| `.drectve` | not needed (verified absent from MinGW objects) | needed |
| In-tree reference implementation | **none** | radlink, 19.7k LOC, exactly this target |
| Spec quality | folklore + binutils `emultempl/pe.em` | Microsoft PE/COFF spec + radlink + lld/COFF |

D3 priced only the debug-info axis and priced MinGW compatibility at zero. Findings R1, R5 and
R6 are all in the region where radlink gives zero help.

Two viable responses:

- **(a)** Keep gnu first, re-scope P3 from 19 tasks to ~25 (auto-import, default-script
  emulation, `.rsrc`, `.CRT$X*`, NRELOC_OVFL, `coff_writer.rs`, COFF `linker-diff`), and move
  `--gc-sections` earlier.
- **(b)** Swap to msvc-first with debug info deferred — which D5 already contemplates — letting
  radlink carry COMDAT, weak externals, base relocs, `.rsrc`, TLS, imports/exports and
  `.drectve` case-for-case, reducing the net-new-without-reference surface to roughly zero.

**What must not happen is shipping P3 as currently written**: its gate's C++ test cannot pass on
the toolchain the gate names, and R4 means the phase can pass its gate while silently producing
incorrect binaries.
