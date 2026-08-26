# Windows / PE-COFF agent guide

This guide owns Windows-specific contributor policy. Read [`../../DESIGN.md`](../../DESIGN.md) §3.1
and the root [`AGENTS.md`](../../AGENTS.md) first.

## Current architecture and references

- Windows links currently use reld's `lld-link` bridge; native PE/COFF code generation is future
  work.
- The byte-identity reference for bridge-only changes is the same pinned `lld-link` backend invoked
  directly with the same effective arguments, inputs, environment, and deterministic options.
- Pin the LLVM/Rust toolchain that supplies `lld-link`. `link.exe` is a compatibility reference,
  not a byte-identity reference, because it has different PE/COFF and PDB policies.

## Required evidence

For routing, argument-forwarding, output-I/O, and other behavior-preserving changes, require the
reld bridge and direct pinned `lld-link` invocation to emit byte-identical PE/COFF artifacts. Run
each side twice first. Enable deterministic linker options where supported.

PE timestamps, PDB identity/age data, paths, or other metadata must not become a blanket exemption.
If an unavoidable field changes between identical invocations, document its exact location and
meaning, normalize only that field, and compare the rest of the executable and associated PDB.

For an intentional output-policy change, compare at least COFF/PE headers, section table and bytes,
imports/exports, base relocations, exception/unwind data, resources, debug-directory entries, and
PDB association. Use `llvm-readobj`, `dumpbin`, or a committed parser with stable output, and add a
focused regression test for the expected delta.

Every executable must run on a native Windows runner with exact exit status, stdout, and stderr.
Test DLL loading or imported functionality when the change touches imports, exports, delay loading,
TLS, or runtime metadata. Cross-compilation alone is not acceptance evidence.

## Consumer acceptance

The three-platform consumer job builds `reld-link.exe` through `setup-soldr`, then links the pinned
Rust and C/C++ consumers through the release executable. Set `RELD_INVOCATION_LOG` for every build
and require a successful JSONL record naming each expected final `.exe` and the `lld-link` bridge.
This is the proof that `reld-link.exe` was actually hit; `PATH`, compiler configuration, or verbose
command text alone is insufficient. Then run `xsv count`, the full pinned PCRE2 CTest suite, and the
C++ name-mangling CTest natively.

## Where Windows policy lives

- Bridge and routing implementation: `crates/reld/src/` and `crates/reld-core/src/`
- Windows CI orchestration: `ci/windows_ci.py`
- Consumer acceptance driver: `ci/consumer_acceptance.py`
- Cross-platform mode validation: `ci/linker_modes.py`
- Benchmark replay and execution oracle: `ci/benchmark_runner.py`
- Platform workflows: `.github/workflows/ci.yml`, `.github/workflows/linker-modes.yml`, and
  `.github/workflows/benchmark-stats.yml`; consumer acceptance runs in
  `.github/workflows/linker-artifacts.yml`

When native PE/COFF code generation lands, update this guide in the same change to name its pinned
behavioral baseline and native structural/differential test suite.
