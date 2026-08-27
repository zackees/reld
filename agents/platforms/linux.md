# Linux / ELF agent guide

This guide owns Linux-specific contributor policy. Read [`../../DESIGN.md`](../../DESIGN.md) §3.1
and the root [`AGENTS.md`](../../AGENTS.md) first.

## Current architecture and references

- Rust 1.95 is the MSRV; normal local and CI builds use the exact 1.95.0 pin, not floating
  `stable`.
- Shared CI runs `python3 ci/check_dependencies.py` before platform builds; an approved dependency
  change must update its baseline in the same reviewed change.
- Fast non-LTO links use `reld`'s native ELF engine inherited from `wild`.
- LTO and other unsupported native capabilities route to the bundled `ld.lld` bridge.
- For inherited native behavior, the artifact reference is the exact pinned `wild` fork commit from
  `DESIGN.md`, or a pinned pre-change `reld` revision when validating a later optimization.
- `bfd`, `lld`, and `mold` are useful compatibility or performance comparators, but they are not
  byte-identity references because each linker has its own ELF layout policy.
- The Linux `reld` executable always uses the exact crates.io `mimalloc-pprof`
  allocator pinned in the workspace manifest and lockfile. Its default configuration
  compiles sampled-profiler hooks out (`MI_PPROF=0`) and leaves internal exact DHAT
  inactive. Use `--features mimalloc-pprof-profile` only for sampled profiling and
  `--features mimalloc-pprof-dhat` only for an exact diagnostic run. Neither profile
  mode replaces the process allocator. The system allocator reference is the pinned
  pre-change baseline, not a supported reld feature.
  See [`../../docs/MIMALLOC-PPROF.md`](../../docs/MIMALLOC-PPROF.md) for
  provenance and updates.

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

For allocator changes, build and replay the same captured ELF link in the default,
sampled-profiling, and exact-DHAT configurations. First establish two-run self-determinism for each
configuration, then compare the default artifact with the pinned pre-change system-allocator
baseline as raw bytes; if a difference appears, preserve it and apply the §3.1 structural and
native-execution requirements before accepting it. Profiling is diagnostic only and must not be
used for a performance claim without the same evidence. Authoritative timing requires the default
hook-free build plus proof that sampled pprof and exact DHAT are runtime-off. Profile-collection
runs are diagnostic only and must never be timing evidence.
Set `RELD_SYSTEM_BASELINE` to the pinned pre-change executable and run
`bash ci/allocator_equivalence.sh` inside the Bosn Linux stack for the focused two-run raw-byte
identity and native-execution proof across the pinned system baseline, default, sampled-pprof, and
exact-DHAT builds. The script clears all supported profiler activation/dump environment controls
before invoking every linker.

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
