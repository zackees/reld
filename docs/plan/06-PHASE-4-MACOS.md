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

`LC_DYLD_CHAINED_FIXUPS` is the modern format and the only one worth implementing. Classic
rebase/bind opcode streams are legacy; emit chained fixups and reject the classic path.

`Acceptance:` `dyld_info -fixups` on the output; the binary runs on macOS 13+.

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

## P4-T9 — Deferred, recorded as scoped gaps

Do not implement in Phase 4; each must error clearly:
Objective-C metadata sections, Swift metadata, universal/fat output binaries, `-object_path_lto`,
`-exported_symbols_list` beyond the simple case.

## P4-T10 — Phase gate

On the `macos-14` runner:

1. C hello-world runs, exit code 42.
2. Rust hello-world via `-Clink-arg=-fuse-ld=` runs on a clean machine, Gatekeeper default.
3. C++ exception across translation units.
4. Differential oracle green against `ld64.lld` over the Mach-O fixture set.
5. `reld-difftest --seeds 500` green.

`Acceptance:` `ci/gate-macos.sh` exits 0.
