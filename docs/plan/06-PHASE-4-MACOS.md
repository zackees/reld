# Phase 4 — Mach-O, aarch64-apple-darwin

Target: `aarch64-apple-darwin`. Apple Silicon, not x86_64 — this forces arm64 relocation work
first, which is the opposite of the other two phases.

Starting point: wild's Mach-O backend is a **skeleton, not a foundation**. It has 40 `todo!()`s,
is arm64-only, is feature-gated off by default, and links only trivial programs. Treat it as a
sketch of the trait impls rather than working code.

Set expectations correctly: Apple's `ld_prime` is already **1.4–2× faster than `ld64.lld`**
(measured by wild's own Mach-O lead). **The macOS pitch is incremental linking and an open
implementation, not raw throughput** — Apple removed `ld64` from Xcode 27, leaving only a closed
binary. Do not plan or publish around beating `ld_prime` on cold-link speed.

---

## P4-T1 — Enumerate and clear the skeleton's `todo!()`s

Per global rule 3, none may survive. Their exact locations in the vendored tree:

| File | Count | Notable sites |
|---|---|---|
| `coff.rs`-equivalent `macho.rs` | 25 | `section_by_name:405`, `symbol_versions:422`, `raw_section_data:466`, `section_data:475`, `section_data_cow:491`, `process_gnu_note_section:576`, `dynamic_tags:582`, every `SectionHeader` method (602–637), every `SectionType` method (646–658), `is_zero_sized_section_content:1052`, `frame_data_base_address:1067`, `take_dynsym_index:1140`, `compute_object_addresses:1147`, `create_dynamic_symbol_definition:1175`, `non_empty_section_loaded:1330`, `allocate_internal_symbol:1607` |
| `macho_aarch64.rs` | 12 | relocation and relaxation handling |
| `macho_writer.rs` | 1 | |
| `args/macho.rs` | 2 | |

Convert each to either a real implementation or an explicit `bail!` naming the construct.

`Acceptance:` `grep -rn 'todo!\|unimplemented!' crates/reld-core/src/macho*` returns nothing.

## P4-T2 — Remove the feature gate

`macho.rs:69-80` currently `bail!`s unless built with `--features macho`. Make it default-on
once P4-T1 lands, matching the COFF decision in P3-T5.

`Acceptance:` `cargo build` (no feature flags) produces a binary that accepts `-flavor darwin`.

## P4-T2b — Kernel- and dyld-enforced invariants ⚠️ added by review (R39)

Verified against xnu `bsd/kern/mach_loader.c` and dyld source. Each is a hard failure, not a
degradation, and none appeared in the original plan:

- **`MH_PIE` is mandatory on arm64.** `pie_required()` returns TRUE for `CPU_TYPE_ARM64`;
  otherwise `LOAD_FAILURE`.
- **`__PAGEZERO` must be exactly `vmsize = 0x100000000`, `initprot = 0`** — the "hard page zero"
  check, else `LOAD_BADMACHO`. `__TEXT` therefore starts at `0x100000000`.
- **`MH_DYLDLINK` is mandatory.** An arm64 `MH_EXECUTE` without it is `LOAD_FAILURE` on a release
  kernel — **static arm64 executables do not run.** Hello world must be dynamic.
- **`LC_MAIN`, not `LC_UNIXTHREAD`.** The pre-`LC_MAIN` path is compiled out except on x86_64
  macOS; an arm64 `LC_UNIXTHREAD` binary maps, fixes up, then `halt("main executable is missing
  LC_MAIN")`. `entryoff = VA(entry) - 0x100000000`.
- **`LC_DYLD_CHAINED_FIXUPS` must point at a valid header even with zero fixups** — `datasize=0`
  is not acceptable, unlike the export trie which may legitimately be empty.
- **`LC_ID_DYLIB` in an executable is a hard error**; missing `LC_LOAD_DYLIB` is a hard error.
- **Dylib ordinals have three incompatible encodings.** `BIND_SPECIAL_DYLIB_*` are
  `SELF=0, MAIN_EXECUTABLE=-1, FLAT_LOOKUP=-2, WEAK_LOOKUP=-3`, but chained fixups use an 8-bit
  field sign-extended only *above 0xF0* (encode `0xFF`/`0xFE`/`0xFD`), and nlist `n_desc` uses
  `EXECUTABLE_ORDINAL=0xFF`, `DYNAMIC_LOOKUP_ORDINAL=0xFE` — unrelated to the signed values.

`Acceptance:` `cargo test --test acceptance -- macho/loader-invariants`

## P4-T3 — Load commands and segment layout

`__PAGEZERO`, `__TEXT`, `__DATA_CONST`, `__DATA`, `__LINKEDIT`. `LC_SEGMENT_64`,
`LC_SYMTAB`, `LC_DYSYMTAB`, `LC_LOAD_DYLINKER`, `LC_MAIN`, `LC_UUID`.

macOS enforces a 16 KiB page size on arm64 and requires `__LINKEDIT` last.

`Acceptance:` `otool -l` parses the output; segment ordering and alignment match a reference
linker.

## P4-T4 — arm64 relocations, stubs, GOT

`ARM64_RELOC_`: `UNSIGNED`, `SUBTRACTOR`, `BRANCH26`, `PAGE21`, `PAGEOFF12`, `GOT_LOAD_PAGE21`,
`GOT_LOAD_PAGEOFF12`, `POINTER_TO_GOT`, `TLVP_LOAD_PAGE21`, `TLVP_LOAD_PAGEOFF12`, `ADDEND`.

Note `ADDEND` is a **prefix relocation** modifying the next entry — a structural difference from
both ELF and COFF that the relocation-list abstraction must accommodate.

Branch islands for images exceeding ±128 MB: wild's `thunks.rs` is already format-generic.

`Acceptance:` one fixture per relocation kind, differentially verified against `ld64.lld`.

## P4-T5 — Chained fixups

`LC_DYLD_CHAINED_FIXUPS` is the modern format. Use **`DYLD_CHAINED_PTR_64_OFFSET` (6)**
exclusively — all `ARM64E` pointer formats carry auth bitfields and a 4-byte stride, and arm64e
is not available to third-party macOS apps.

⚠️ **"Reject the classic path" is unresolved and must be settled by measurement** (R42). Chained
fixups are the *default* only above a deployment-target threshold, and sources disagree on
whether ld64's threshold is macOS 11 or 12. rustc's default `aarch64-apple-darwin` deployment
target may fall below it, in which case rejecting the legacy path breaks `cargo build`.

**Resolve empirically on the pinned reference toolchain** — `otool -l` on a real `cargo build`
output — before committing. This contradicts P4-T7's platform-version handling if left
unresolved.

`Acceptance:` `dyld_info -fixups` on the output; the binary runs on macOS 13+.

## P4-T5b — `__unwind_info` construction ⚠️ added by review (R38) — was missing entirely

Compact unwind is **not** `.eh_frame`, and this is not a passthrough. The linker consumes
`__compact_unwind` input sections and **builds** `__unwind_info`, a two-level compressed table.
Without it, C++ exceptions do not unwind — yet the original plan mentioned `__unwind_info` only
in the incremental document.

Hard structural constraints: **511 regular / 1021 compressed entries per second-level page**, a
**3-personality cap**, a required trailing sentinel entry, and a rule that LSDA-bearing entries
cannot be folded with their neighbours. The linker must also copy `__eh_frame` into
`__TEXT,__eh_frame` so entries can carry the MODE_DWARF hint.

This is a substantial subsystem, comparable to P4-T5, not a detail of it.

`Acceptance:` a C++ fixture throwing across a translation-unit boundary returns 42, and
`unwinddump` reports a well-formed table.

## P4-T5c — Static initializers

Dispatch is by **section type, not a header flag** — `MH_HAS_INIT_OFFSETS` does not exist
(R40). Use `S_MOD_INIT_FUNC_POINTERS` (0x09); `S_INIT_FUNC_OFFSETS` (0x16) is the newer
chained-fixups-era form. `__mod_init_func` is the more compatible choice for a from-scratch
linker. Rust hello-world emits no initializer section at all, so this is C++-driven.

`Acceptance:` a C++ fixture with a global constructor observes it having run.

## P4-T6 — `.tbd` stub dylibs

Linking against the shared cache means never touching a real `libSystem.dylib` — the SDK ships
YAML text stubs. Parse `.tbd` v4 (and v5 JSON if present in the pinned SDK) for exported symbol
lists and re-export chains. wild has `macho_stub_library.rs` as a starting point.

`Acceptance:` a fixture linking against `libSystem` resolves `printf` from the SDK `.tbd`.

## P4-T7 — Platform version and SDK plumbing

`LC_BUILD_VERSION` with platform, minos, and sdk. `-platform_version` argument. dyld rejects
binaries whose minos exceeds the running OS, so this must be right, not approximate.

`Acceptance:` `vtool -show` reports the expected platform triple.

## P4-T8 — Ad-hoc code signature (D6)

**Mandatory** — unsigned arm64 binaries do not execute on Apple Silicon. Generate the
`LC_CODE_SIGNATURE` blob in-process: a `SuperBlob` containing a `CodeDirectory` with SHA-256
page hashes over the image, plus an empty requirements blob. Ad-hoc means an empty signature
slot and no identity.

Do not shell out to `codesign` — an external process in the hot path defeats the premise.

Ordering trap: the signature covers the image including the load commands, so its size must be
reserved during layout before its contents can be computed.

`Acceptance:` `codesign -v` accepts the binary and it executes on a clean Apple Silicon machine
with Gatekeeper at defaults.

## P4-T8b — Accept the flags rustc actually passes

Measured from rustc's `aarch64-apple-darwin` link line, and absent from the original plan:

- **`-dead_strip` is passed on every default `cargo build`** (`GccLinker::gc_sections`:
  `if is_like_darwin { self.link_arg("-dead_strip") }`). clang does *not* add it. **Ignoring it
  is correct** — keeping everything is always a valid superset — but it must be accepted, and
  implementing it *badly* is the real hazard.
- `-arch arm64`, `-platform_version macos <min> <sdk>`, `-syslibroot <sdk>`, `-pie`,
  `-rpath @loader_path/...`, `-force_load`, `-exported_symbols_list`.
- Note rustc links through `cc` by default (`LinkerFlavor::Darwin(Cc::Yes, Lld::No)`), passing
  `-arch arm64 -mmacosx-version-min=<target>` and `SDKROOT` in the *environment*.

`Acceptance:` `cargo test --test acceptance -- macho/rustc-flags`

## P4-T9 — Deferred, recorded as scoped gaps

Do not implement in Phase 4; each must error clearly:
Objective-C metadata sections, Swift metadata, universal/fat output binaries, `-object_path_lto`,
`-exported_symbols_list` beyond the simple case.

## P4-T10 — Phase gate

On the `macos-14` runner:

1. C hello-world runs, exit code 42.
2. Rust hello-world via the **`-B<fakedir>`** mechanism (not `-fuse-ld=`) executes — i.e. **AMFI
   accepts it**. Note Gatekeeper and AMFI are different mechanisms: `spctl` would reject an
   ad-hoc-signed binary that nonetheless runs fine, and ad-hoc signing does not satisfy
   Gatekeeper. The gate tests execution, which is what a locally-built dev binary faces.
3. C++ exception across translation units (exercises P4-T5b `__unwind_info`).
4. C++ global constructor observed to have run (P4-T5c).
5. `.tbd` stub resolution against the SDK `libSystem` (P4-T6).
6. `codesign -v` accepts the output (P4-T8).
7. Differential oracle green against `ld64.lld`, **with `--coverage` meeting its floor and at
   least one Mach-O `RELD_MALFUNCTION` site proving the oracle looks** (D17).
8. `reld-difftest --seeds 500` green.

`Acceptance:` `ci/gate-macos.sh` exits 0.
