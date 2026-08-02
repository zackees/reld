# Acceptance testing — all four targets

This is the contract for "it works." A phase is not done because the code is written; it is done
when the commands in this document exit 0 on CI.

---

## 1. The five-layer oracle

No single check catches linker bugs. Each layer catches a class the others miss, and the layers
are ordered by cost-to-build, cheapest first.

| Layer | What it catches | What it misses |
|---|---|---|
| **L1 Structural** — parse the output with `object`, assert on typed fields (`ExpectSym`, `ExpectSection`, entry point, data directories) | Malformed output, missing symbols, wrong section flags | Anything semantically wrong that is still well-formed |
| **L2 Execution** — run the binary, **require exit code 42** | Wrong relocation targets that actually crash; wrong initialization order | Bugs on paths the fixture does not execute |
| **L3 Differential** — `reld-diff` vs. a reference linker, comparing recovered *symbol-level* relocation targets via the `.layout` sidecar | Wrong relocation targets that happen not to crash. **The highest-value layer.** | Bugs the reference linker shares; anything outside `.text`/`.data` |
| **L4 External suite** — mold's 518 shell tests (Linux) with a skip-list ratchet | Feature gaps we did not think to test | Non-Linux formats |
| **L5 Corpus** — real projects linked and their own test suites run | Integration failures no synthetic fixture models | Anything not in the corpus |

Two rules that make these worth having:

- **L2 uses exit code 42, not 0.** A binary that segfaults, exits 0 spuriously, or runs the
  wrong `main` must all fail. This is wild's `EXIT_SUCCESS: i32 = 42`.
- **L3 never byte-compares.** Reproducible output is an explicit non-goal and layout differences
  are legitimate; a byte-diff oracle would be both wrong and unusably noisy. See D12.

**The oracle itself is tested** (P1-T5): `RELD_MALFUNCTION=<id>` injects a deliberate bug and the
suite *requires* that L3 reports it. Without this, "the differ found nothing" and "the differ is
broken" look identical.

---

## 2. Per-target matrix

| | linux-gnu x86_64 | windows-gnu x86_64 | windows-msvc x86_64 | macos arm64 |
|---|---|---|---|---|
| Runner | `ubuntu-latest` | `windows-latest` + MSYS2 UCRT64 | `windows-latest` | `macos-14` |
| Format | ELF | PE/COFF | PE/COFF | Mach-O |
| Arg dialect | GNU ld | GNU ld (`-m i386pep`) | `link.exe` | ld64 |
| Reference linkers | `ld.bfd`, `ld.lld`, `mold`, `wild` | `ld` (binutils-mingw), `ld.lld` | `link.exe`, `lld-link` | `ld` (ld_prime), `ld64.lld` |
| Debug format | DWARF | DWARF | per D5 | DWARF |
| Debugger test | gdb | gdb (MSYS2) | per D5 | lldb |
| Exceptions | `.eh_frame` | SEH `.pdata`/`.xdata` | SEH `.pdata`/`.xdata` | `__unwind_info` |
| Signing | — | — | — | **ad-hoc, mandatory** |
| Phase | P2 | P3 | P5 | P4 |

Reference linker versions are **pinned** and recorded in published results, per `DESIGN.md` §6.

### Toolchain-integration test — **per platform, not one mechanism** (R34)

The original text claimed one mechanism worked "across all four." It works on one.

| Target | Mechanism |
|---|---|
| linux-gnu, windows-gnu | `$CC -B<dir>` with a **wrapper script** named `ld` (`#!/bin/bash\nexec <reld> "$@"`). Not a symlink — wild's own comment: *"lld requires that it's invoked as `ld.lld` to work properly."* |
| macos | `$CC -B<dir>` with a wrapper named `ld` |
| windows-msvc | **`-B` does not exist for `cl.exe`.** `link.exe` is selected via the `LINK` environment variable or `/LINKER:`; rustc uses `-Clinker=`. |

`-fuse-ld=<path>` is **not** usable: measured, GCC accepts only `bfd|gold|lld|mold` and errors on
a path (absolute paths are a clang-only extension). MinGW's driver is GCC. Any phase doc using
`-fuse-ld=` is wrong.

Note rustc's windows-gnu line invokes `x86_64-w64-mingw32-gcc`, not `gcc`, so the shim directory
must be reachable from that driver.

---

## 3. Per-target acceptance gates

Each is a script in `ci/`. All are required for the corresponding phase to close.

### `ci/gate-linux.sh` (P2)
1. Full ELF fixture suite, x86_64 + aarch64, L1+L2+L3 green vs. `ld.bfd`.
2. **Self-host**: reld links reld; the resulting binary links the suite again (two generations).
3. mold's external suite green with a skip list, and the `expect_failure` ratchet passing.
4. `reld-difftest --seeds 500`.
5. Undefined-symbol diagnostic names the referencing object and demangles.

### `ci/gate-wingnu.sh` (P3)
1. C hello-world, exit 42.
2. `cargo build --target x86_64-pc-windows-gnu` of a nontrivial crate → running binary.
3. **gdb batch-mode breakpoint by file:line hits.** Scripted, not manual.
4. C++ exception thrown and caught across a translation-unit boundary.
5. DLL round-trip: build a DLL + import lib with reld, link an exe against it with reld, run.
6. `__thread` TLS fixture.
7. Global constructor observed to have run.
8. L3 green vs. `ld.lld` across the COFF fixture set.
9. `reld-difftest --seeds 500`.

### `ci/gate-macos.sh` (P4)
1. C hello-world, exit 42.
2. Rust hello-world running **on a clean machine with Gatekeeper at defaults** — the real test
   of P4-T8, since an improperly signed binary is killed by the kernel, not by the linker.
3. `codesign -v` accepts the output.
4. C++ exception across translation units (`__unwind_info`).
5. `.tbd` stub resolution against the SDK `libSystem`.
6. L3 green vs. `ld64.lld`.
7. `reld-difftest --seeds 500`.

### `ci/gate-winmsvc.sh` (P5)
1. `cl.exe` hello-world, exit 42.
2. `cargo build --target x86_64-pc-windows-msvc` → running binary.
3. Debug story per the D5 decision, with a scripted test.
4. C++ static initializers run in `.CRT$XC*` order.
5. L3 green vs. `lld-link`.
6. `reld-difftest --seeds 500`.

---

## 4. The corpus (L5), published as a pass rate

~15 pinned projects, linked by reld in CI, with their own test suites run where they have one.
Published to the `benchmark-stats` branch under the same honesty policy as the benchmarks: **a
regressing number is still published.**

Chosen for the stressors they exercise, not for popularity:

| Stressor | Representative |
|---|---|
| Heavy C++ templates, COMDAT volume | LLVM subset, or `fmt` + `catch2` |
| TLS models | a threaded C benchmark |
| Symbol versioning (ELF) | anything glibc-linked |
| Static + PIE variants | busybox / musl builds |
| Rust with build scripts and proc macros | `ripgrep`, `tokio` |
| Windows API surface | a Win32 GUI sample |
| Objective-C-free macOS C/C++ | `sqlite`, `zstd` |
| Exceptions + RTTI | a C++ test suite |

A link-success table is more persuasive at this stage than any speed chart, and it can start
publishing long before the speed column has content.

---

## 4b. Gaps the review found in this document (R37) — all required

- **Self-determinism.** D12 rules out *cross-linker* byte reproducibility. It does **not** rule
  out: same reld, same inputs, N runs, `--threads 1` vs `--threads 16` → identical bytes.
  Upstream carries a dozen "tie-break for determinism" comments (`elf.rs:863`, `:4700`,
  `gdb_index.rs:498`, `layout.rs:1239`…), each a latent race with no regression guard. For a
  *parallel incremental* linker this is the highest-probability class of shipped bug and **no
  layer L1–L5 fires on it.** Do not let D12's wording block this test — it is a different
  property.
- **Performance and RSS regression gate.** For a linker whose pitch is the dev loop, a 3×
  link-time regression currently passes every gate in this document. `reld-bench` and the
  benchmark-stats pipeline already exist; wire a threshold.
- **"No output on failure."** A linker that writes a truncated binary and *then* exits nonzero
  passes every gate here, and `make` will treat the target as up to date. Assert `!out.exists()`
  on every `ExpectError` path. One line.
- **Scale.** Nothing specifies workload *size*. The bugs that ship are threshold bugs: >64K
  sections (COFF `NumberOfSections` is `u16`), >2 GB output, >64K relocations in one COFF
  section (`IMAGE_SCN_LNK_NRELOC_OVFL`). None appear in a 100-seed synthetic run.
- **Re-link over an existing output.** Especially relevant given the product is incremental and
  mold's measured 300 ms win comes from overwriting rather than recreating.

## 5. What we deliberately do NOT do in v0

Recorded so these read as decisions rather than oversights.

- **No qemu cross-architecture matrix.** mold runs ~7,400 cases across 18 architectures with
  vendor-URL toolchain downloads; that is correct for a mature linker and a millstone for a new
  one. Structure the matrix so adding a target is a one-line change (mold's `add_target()` +
  `arch-<arch>-*.sh` filename convention), then defer.
- **No fuzzing in v0.** Malformed-input handling is covered by hand-written `ExpectError`
  fixtures. Neither wild nor mold has a fuzzer. Revisit once the formats are stable — the
  requirement will be *clean error, never panic, never wrong binary*.
- **No byte-reproducibility tests.** Explicit non-goal.
- **No release-quality assertions** — no PGO/BOLT interaction, no reproducibility. Non-goals per
  `DESIGN.md`.
- **No LTO acceptance tests yet.** (Thin)LTO is a **stretch goal, sequenced after the fast
  linker on all platforms** (`DESIGN.md` §3.1) — not a non-goal. What v0 *must* test is that LTO
  flags produce a clear diagnostic rather than a silent mislink: one fixture per platform
  passing `-flto` / `--plugin` / `/LTCG` and asserting the error names the feature. Note the fork
  inherits wild's linker-plugin LTO (with known issues) on ELF, so P2 must not regress it — add
  a smoke test pinning current behaviour rather than deleting the code.

---

## 6. Anti-patterns to avoid, drawn from the reference projects

- **Ignore-list rot.** wild's `apply_wild_defaults()` hard-codes ~60 permanently-disabled
  assertions, several admitting the divergence is not understood. Ours require a tracking issue
  ID per entry, and **CI fails when an ignore is no longer needed**. An ignore that silently
  stops being necessary is an assertion you have lost.
- **Directive sprawl.** wild's harness DSL has ~90 directives including two deprecated ones.
  Ours is frozen at 18 and the parser **rejects unknown directives**.
- **Silent skips.** A capability-gated suite that skips everything reports green.
  `RELD_VERIFY_PLATFORM_REQUIREMENTS=1` asserts nothing was skipped.
- **`readelf | grep` as an assertion language.** mold's idiom is fragile against binutils
  versions and locale. Parse with a library, assert on typed fields.
- **No unit tests.** mold has none; linker-script parsing, version-script globbing, and
  expression evaluation are pure functions and should not require a 30-second
  cross-compile-and-run round trip. wild's 132 `#[test]`s are inherited — keep them.
