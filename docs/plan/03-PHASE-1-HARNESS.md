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

**REVISED after review 02 (R32).** The original "freeze at 18" was both miscounted (the list had
20 names) and **mutually exclusive with P2-T1's requirement that wild's 213 fixtures pass**.
Measured: **77 distinct directives in use across 247 fixture files; 164 files (66%) use at least
one directive outside the proposed set.** `ReferenceLinkers:` alone has 126 uses, and P1-T5's own
`Malfunction:` directive was not in the set — so P1-T5's fixtures would fail to parse under
P1-T1's own reject-unknown parser, within a single phase.

**Freeze at the following 32.** Still far below wild's 77-in-use, and still rejecting the two
deprecated spellings (`SkipLinker:`, `EnableLinker:`), but expressible against the inherited
corpus:

```
Config:{name}[:{inherits}]     AbstractConfig:{name}[:{inherits}]
Object:{file}[:args]           Relocatable:{file}[:args]
Archive:{file}                 Shared:{file}[:args]
LinkerScript:{file}
CompArgs:...                   LinkArgs:...          LinkerDriver:gcc|clang|none
Compiler:gcc|g++|clang|clang++
Mode:static|dynamic            Arch:{a}[,{b}]        SkipArch:{a}[,{b}]
Platform:{elf|coff|macho}[,..] ReferenceLinkers:{names}
ExpectSym:{name} [props]       NoSym:{name}
ExpectDynSym:{name} [props]    NoDynSym:{name}
ExpectSection:{name}           NoSection:{name}      ExpectSectionBytes:{name}=0x{hex}
ExpectDynamic:{tag}            ExpectError:{regex}   Contains:{string}
RunEnabled:{bool}              ExpectRunOutputEmpty:{bool}
DiffEnabled:{bool}             DiffMatchAny:{bool}
DiffIgnore:{key} #{issue} [arch={a}[,{b}...]]
Malfunction:{id}               Requires{Glibc,NightlyRustc,LinkerPlugin,...}:{bool}
```

`Requires*` is a family, not one directive — and it is load-bearing: `RELD_VERIFY_PLATFORM_
REQUIREMENTS` (P1-T8) exists precisely to assert that `Requires*`-gated skips were unnecessary.
Delete the family and the escape hatch has nothing to verify.

`Relocatable:` compiles its inputs, performs a linker-specific partial link (`-r` for ELF), checks
that intermediate for linker diagnostics and object validity, and feeds it to the final link.
`ExpectRunOutputEmpty:true` keeps the exit-code-42 oracle and additionally requires exact empty
stdout and stderr from normal executable execution. It is rejected with `RunEnabled:false` or
`RunDynSym:` rather than becoming a silent no-op.

Directives still deliberately dropped, with their fixture cost accepted: `TestUpdateInPlace:`
(17), `ExpectProgramHeader:` (16), `AutoAddObjects:`, `RemoveSection:`, `DriverMode:`,
`SecEquiv:`, `Variant:`, `MaxThunks:`, the `.gdb_index` family.

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

⚠️ **The word "port" is wrong for non-ELF, and the failure mode is silently green** (R30).
`header_diff.rs:249-279` matches `object::File::Elf64` and falls through `_ => {}`; for COFF or
Mach-O `values` is empty, empty compares to empty, and the test **passes**. Mach-O coverage in
the whole 10,195-LOC crate is 16 lines; COFF is zero references. So "~700 LOC" is the size of
the *ELF* implementation — the COFF and Mach-O field tables are new code.

- **P1-T3a — header/structural diff, ELF.** Port `header_diff.rs`. Format-dispatched behind a
  trait from the start, and **the trait's default impl must `bail!`, never return empty** — an
  unimplemented format must fail loudly, not silently pass.
- **P1-T3b — relocation/disassembly diff, ELF.** Port `asm_diff.rs`.
- **P1-T3c — coverage measurement (D17).** `linker-diff` already has `--coverage`. Wire it up,
  publish per format, and gate on a floor. Measured ELF coverage is only **~50–70%** of
  relocations for C (symbol-name-keyed; duplicated locals dropped; `SUPPORTED_SECTION_KINDS`
  excludes `.rodata`, where most absolute relocations live).

**Per-format instantiation is a separate, phase-sized task in each backend phase, not a P1
deliverable** — realistically 1,500–2,500 new LOC each. On COFF specifically, `object` reports
`implicit_addend: true` with `addend` holding only the fixed PC bias (`REL32 => -4`), while the
real addend lives in the section bytes; `asm_diff` treats `rel.addend()` as the complete
symbolic offset at six sites, so a naive instantiation makes **every recovered referent wrong**.
Mach-O additionally needs `SUBTRACTOR` pairs and `ARM64_RELOC_ADDEND` (an addend delivered as a
separate relocation record), neither expressible in the current chain model.

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

Port wild's `malfunction.rs`. `RELD_MALFUNCTION=<id>` activates a deliberately-wrong code path;
a fixture declaring that malfunction **requires** that `reld-diff` reports a problem, and
snapshot-tests the report text.

This is how you know the oracle works. Without it, a differ that silently reports nothing looks
identical to a differ that finds no bugs.

Three corrections from review (R36):

- **Upstream has no injection site in relocation application.** All five sites are in
  `elf_aarch64.rs`, `elf_writer.rs`, `elf_x86_64.rs`, and four of the five are
  *relaxation-suppression*, meaningful only to an oracle that infers relaxations. The
  `elf_writer.rs:431` header-corruption site is the only portable exemplar. Write a
  relocation-application site ourselves — it is the one that matters most and has no template.
- **`cfg!(debug_assertions)` compiles the machinery out in a release profile.** The CI matrix
  must state its profile explicitly, or this validation layer silently vanishes on any release
  job.
- **D17: at least one injection site per format is a phase-exit gate** for P3, P4 and P5.
  Otherwise the COFF and Mach-O oracle paths ship completely unvalidated — which is exactly
  where bugs are most likely.

`Acceptance:` `cargo test --test acceptance malfunction` — each malfunction fixture fails when
the malfunction is active and passes when it is not.

## P1-T6 — External suite ratchet

**Linux/ELF only** — mold's suite produces and inspects ELF exclusively, uses `/proc`, and
assumes bash. This task therefore belongs to the ELF workstream even though it is listed in P1.

Mechanism, **corrected** (R34): not symlinks. wild explicitly uses **wrapper shell scripts**,
with the reason in a comment — *"we can't just create a symlink, since lld requires that it's
invoked as `ld.lld` to work properly."* Create a directory of `#!/bin/bash\nexec <reld> "$@"`
shims and run each mold test with that directory as cwd so `$CC -B.` picks up reld as `ld`.

Two counting corrections:

- **407, not 518.** 111 are `arch-*` tests requiring qemu, which P1-T8 bans. They need explicit
  exclusion from the ratchet, exactly as wild carves them out.
- **mold's `skip()` exits 0**, so a skipped test "passes." Combined with an `expect_failure`
  trial that fails on success, any skip-listed test that *skips* on our runner (no musl, no
  ifunc, probe failure) **spuriously fails the ratchet**. The skip list must distinguish
  "unsupported by reld" from "not runnable here," and only the former gets an `expect_failure`
  trial.

**Provenance:** wild obtains the suite as a git submodule (`external_test_suites/mold`),
uninitialized in our checkout. reld has no `.gitmodules`. This task must pin and vendor it
explicitly. Note there is a **second** submodule, `wild/tests/bins` (prebuilt binary test
inputs), which a chunk of the 213 inherited fixtures depend on and which no task mentions.

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
