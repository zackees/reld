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

### Toolchain-integration test (all four)

The mechanism that proves reld is usable, not just correct. Use mold's proven approach — a
directory containing a binary named `ld` (or `ld.lld` / `link.exe`) symlinked to reld, with the
compiler invoked as `$CC -B<dir>`. This works across gcc, clang, and rustc without depending on
`-fuse-ld=` accepting arbitrary names.

```
cargo build --target <triple> --config \
  'target.<triple>.rustflags=["-Clink-arg=-B/path/to/fakedir"]'
```

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
- **No release-quality assertions** — no LTO, no PGO/BOLT interaction. Non-goals per `DESIGN.md`.

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
