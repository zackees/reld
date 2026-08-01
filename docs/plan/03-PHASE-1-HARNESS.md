# Phase 1 — Acceptance-test infrastructure

**This phase ships before any new format code.** The differential oracle must be proven against
a known-good linker while reld's ELF backend is still just wild's — otherwise, the first time it
disagrees with a reference linker on COFF, you cannot tell whether the linker or the harness is
wrong.

Everything here is adapted from patterns already proven in wild and mold. Sources:
`.extern-repos/wild/wild/tests/integration_tests.rs` (7092 LOC),
`.extern-repos/wild/linker-diff/` (10,195 LOC), `.extern-repos/mold/test/` (518 shell scripts).

---

## P1-T1 — Port the integration harness, with a frozen directive set

Port `wild/tests/integration_tests.rs` to `crates/reld/tests/acceptance.rs`. It is a
`harness = false` test target using `libtest_mimic` to synthesize trials at runtime from source
files annotated with `//#Directive: Args` comments.

**Freeze the directive set at the following 18.** wild has ~90, including two explicitly
deprecated ones (`SkipLinker:`, `EnableLinker:`) superseded by `ReferenceLinkers:`. That count is
a symptom of one-at-a-time accretion. Ours:

```
Config:{name}[:{inherits}]     AbstractConfig:{name}[:{inherits}]
Object:{file}[:args]           Archive:{file}        Shared:{file}[:args]
CompArgs:...                   LinkArgs:...          LinkerDriver:gcc|clang|none
Mode:static|dynamic            Arch:{a}[,{b}]        SkipArch:{a}[,{b}]
Platform:{elf|coff|macho}[,..]
ExpectSym:{name} [props]       NoSym:{name}          ExpectSection:{name}   NoSection:{name}
ExpectError:{regex}
RunEnabled:{bool}              DiffEnabled:{bool}    DiffIgnore:{key}
```

The parser **must reject unknown directives** with an error naming the file and line. wild's
spec lives in a doc-comment and will drift; ours lives in `docs/plan/03-PHASE-1-HARNESS.md` and
is enforced by the parser.

`Acceptance:` `cargo test -p reld --test acceptance -- --list` enumerates trials; an unknown
directive in a fixture fails the run with a precise message.

## P1-T2 — Layout sidecar

The differential oracle depends on the linker telling it where each input section landed.
Inherited from wild as `RELD_WRITE_LAYOUT=1` → `crates/reld-core/src/file_writer.rs:415`
(`write_layout`), schema in `crates/reld-layout-schema/`.

Task: verify it survives the rename, and **extend the schema to be format-neutral** — it is
currently ELF-shaped. It must be able to describe a COFF contribution (object index, section
index, output section, RVA) and a Mach-O one.

`Acceptance:` `RELD_WRITE_LAYOUT=1 reld -o out a.o && test -f out.layout`

## P1-T3 — `reld-diff`: the semantic differential oracle

Port `linker-diff`. Its method (from `linker-diff/src/asm_diff.rs`, 4062 LOC): take the *input*
object's relocation list as ground truth; use the `.layout` sidecar to find where each linker
placed that input section; decode what each linker actually wrote, inferring which relaxation
was applied by testing candidates against the surrounding bytes; reverse the relocation
arithmetic to recover the symbolic target; compare **symbol-level meaning**, not addresses.

**Build it in two stages. Do not attempt the disassembler first.**

- **P1-T3a — header/structural diff (~700 LOC, catches an enormous class of bugs).** Port
  `header_diff.rs`: field-by-field comparison of the file header, dynamic/data-directory
  entries, and per-section header fields. Format-dispatched behind a trait from the start — do
  **not** let per-format special-casing leak into shared code the way wild's `PlatformKind`
  checks do.
- **P1-T3b — relocation/disassembly diff.** Port `asm_diff.rs`. ELF-only initially; the COFF
  instantiation lands in Phase 3.

**Ignore-list policy — this is a real requirement, not a nicety.** wild's
`apply_wild_defaults()` (`linker-diff/src/lib.rs:145`) hard-codes ~60 permanently-disabled
assertions, several with comments admitting the divergence is not understood. Ours:

- every ignore entry requires a tracking issue ID in the same line;
- CI fails when an ignore entry is **no longer needed** (the mold-skip-list ratchet, applied to
  ignores). An ignore that silently stops being necessary is an assertion you have lost.

`Acceptance:` `cargo test -p reld-diff` green; `reld-diff a.elf b.elf` produces a report.

## P1-T4 — Execution oracle with the magic exit code

Run the produced binary; **require exit code 42** (wild's `EXIT_SUCCESS: i32 = 42`,
`integration_tests.rs:2642`). A binary that segfaults, exits 0 spuriously, or runs the wrong
`main` must all fail loudly. Timeout 10s.

Two known traps to handle up front:
- **ETXTBSY**: writing an executable from a multithreaded runner and immediately exec'ing it is
  racy on Linux. Port wild's `spawn_with_retry` (`integration_tests.rs:~2745`).
- **Windows**: the analogous sharing-violation on a just-written `.exe`; retry on
  `ERROR_SHARING_VIOLATION`.

`Acceptance:` a fixture that returns 0 instead of 42 must FAIL the suite.

## P1-T5 — Mutation testing of the oracle

Port wild's `malfunction.rs`. `RELD_MALFUNCTION=<id>` activates a deliberately-wrong code path
(compiled only under `debug_assertions`); a fixture declaring that malfunction **requires** that
`reld-diff` reports a problem, and snapshot-tests the report text.

This is how you know the oracle works. Without it, a differ that silently reports nothing looks
identical to a differ that finds no bugs. Port at least the injection sites in relocation
application and output-header writing.

`Acceptance:` `cargo test --test acceptance malfunction` — each malfunction fixture fails when
the malfunction is active and passes when it is not.

## P1-T6 — External suite ratchet

Run **mold's 518 shell tests** against reld on Linux. Mechanism (from
`wild/tests/external_tests/mold_tests.rs`): create a directory of fake linkers — symlinks named
`ld`, `ld.lld`, `mold` pointing at the reld binary — and run each mold test with that directory
as cwd, so `$CC -B.` picks up reld as `ld`.

The ratchet is the point: `mold_skip_tests.toml` lists known-unsupported tests with a reason per
group, **and every skipped test also registers an `expect_failure` trial that fails CI if the
test starts passing.** This converts an external suite from a wall into a progress meter.

`Acceptance:` `cargo test --test external -- mold` green, with a skip list and a passing
`expect_failure` set.

## P1-T7 — Wire the existing workload generator to the differential oracle

`crates/reld-testkit/` already generates seeded synthetic C workloads and was explicitly
designed for three consumers (benchmarks, stress, differential). Only the differential consumer
is missing.

Add `reld-difftest`: generate a workload from a seed, link with reld and with a reference
linker, run both, compare exit code and stdout. **Not a byte comparison** (D12). A failure
reports a single `u64` seed that reproduces the exact input set.

`Acceptance:` `cargo run --bin reld-difftest -- --seeds 100` green; injecting a known bug makes
it fail with a reproducing seed.

## P1-T8 — CI matrix

Four native runners. No qemu in v0 — mold's 18-architecture qemu matrix with vendor-URL
toolchain downloads is correct for a mature linker and a millstone for a new one. Structure the
matrix so adding a target is a one-line change, then defer.

| Job | Runner | Reference linkers |
|---|---|---|
| linux-gnu x86_64 | `ubuntu-latest` | `ld.bfd`, `ld.lld`, `mold`, `wild` |
| windows-gnu x86_64 | `windows-latest` + MSYS2 UCRT64 | `ld` (binutils-mingw), `ld.lld` |
| windows-msvc x86_64 | `windows-latest` | `link.exe`, `lld-link` |
| macos arm64 | `macos-14` | `ld` (ld_prime), `ld64.lld` |

Reference linker versions must be **pinned** and recorded in the published results, per the
benchmarking policy in `DESIGN.md` §6.

Also port wild's `RELD_VERIFY_PLATFORM_REQUIREMENTS=1` — an escape hatch that asserts no test
silently skipped. A capability-gated suite that quietly skips everything reports green.

`Acceptance:` all four jobs green on a PR, with a per-job count of tests run and skipped.
