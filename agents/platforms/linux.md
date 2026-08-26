# Linux / ELF agent guide

This guide owns Linux-specific contributor policy. Read [`../../DESIGN.md`](../../DESIGN.md) §3.1
and the root [`AGENTS.md`](../../AGENTS.md) first.

## Current architecture and references

- Fast non-LTO links use `reld`'s native ELF engine inherited from `wild`.
- LTO and other unsupported native capabilities route to the bundled `ld.lld` bridge.
- For inherited native behavior, the artifact reference is the exact pinned `wild` fork commit from
  `DESIGN.md`, or a pinned pre-change `reld` revision when validating a later optimization.
- `bfd`, `lld`, and `mold` are useful compatibility or performance comparators, but they are not
  byte-identity references because each linker has its own ELF layout policy.

## Required evidence

For performance-only native ELF changes, replay the same captured link with identical effective
arguments and environment, then require raw file identity between:

1. serial and parallel implementations when both exist;
2. mmap, buffered, and direct-write output modes when applicable; and
3. the candidate and its pinned `reld`/`wild` baseline.

Run each side twice to verify self-determinism. Compare raw bytes or cryptographic hashes first. If
they differ, preserve the artifacts and inspect the first differing offsets; do not accept execution
success as sufficient.

When an intentional ELF policy change makes identity inappropriate, record the expected changed
fields and compare at least ELF headers, program headers, section headers, section contents,
symbols, dynamic entries, relocations, notes/build ID, and loadable-segment permissions. Use
`readelf`, `llvm-readobj`, or an equivalent committed parser with stable output. Add a focused test
for the policy delta.

Every produced executable must also run natively with exact exit status, stdout, and stderr. Keep
the randomized `reld-difftest` execution comparison as a supplemental oracle; it does not replace
artifact comparison.

## Consumer acceptance

The three-platform consumer job builds the host-named `reld` ELF driver through `setup-soldr`, then
links the pinned Rust and C/C++ consumers through that release executable. Set
`RELD_INVOCATION_LOG` for every build
and require a successful JSONL record naming each expected final artifact and the native ELF route.
This invocation record is mandatory even when verbose compiler output appears to name reld. Then
run `xsv count`, the full pinned PCRE2 CTest suite, and the C++ name-mangling CTest natively.

## Where Linux policy lives

- Native implementation: `crates/reld-core/src/`
- Differential runner: `crates/reld-testkit/src/bin/reld-difftest.rs`
- Benchmark replay and execution oracle: `ci/benchmark_runner.py`
- Consumer acceptance driver: `ci/consumer_acceptance.py`
- Linux acceptance and differential CI: `.github/workflows/ci.yml`
- Three-platform consumer acceptance: `.github/workflows/linker-artifacts.yml`
- Published benchmark matrix: `.github/workflows/benchmark-stats.yml`

Do not publish a speed result from a performance-only change when artifact identity has not been
checked. If the current harness lacks the required gate, add the gate or clearly mark the result as
diagnostic rather than accepted.
