# macOS / Mach-O agent guide

This guide owns macOS-specific contributor policy. Read [`../../DESIGN.md`](../../DESIGN.md) §3.1
and the root [`AGENTS.md`](../../AGENTS.md) first.

## Current architecture and references

- macOS links currently use reld's `ld64.lld` bridge; native Mach-O code generation is future work.
- The byte-identity reference for bridge-only changes is the same pinned `ld64.lld` backend invoked
  directly with the same effective arguments, inputs, environment, and deterministic options.
- Pin the LLVM/Rust toolchain that supplies `ld64.lld`. Apple's linker is a compatibility and
  performance reference, not a byte-identity reference, because its Mach-O layout policy differs.

## Required evidence

For routing, argument-forwarding, output-I/O, and other behavior-preserving changes, require the
reld bridge and direct pinned `ld64.lld` invocation to emit byte-identical Mach-O artifacts. Run
each side twice before comparing candidate to reference.

UUIDs, code-signature material, paths, or other metadata must not become a blanket exemption. If an
unavoidable field changes between identical invocations, document its exact load command or byte
range, normalize only that field, and compare every other byte. Prefer deterministic generation
over normalization.

For an intentional output-policy change, compare at least the Mach header, load commands, segment
and section layout and bytes, symbol and string tables, relocations/fixups, exports, dylib and rpath
commands, unwind information, UUID, and code-signature metadata. Use `otool`, `llvm-objdump`,
`llvm-readobj`, or a committed parser with stable output, and add a focused regression test for the
expected delta.

Every executable must run on a native macOS runner with exact exit status, stdout, and stderr. Test
dynamic-library loading when the change touches dylibs, rpaths, exports, fixups, or signing. A link
performed or inspected only on Linux is not acceptance evidence.

## Where macOS policy lives

- Bridge and routing implementation: `crates/reld/src/` and `crates/reld-core/src/`
- Cross-platform mode validation: `ci/linker_modes.py`
- Benchmark replay and execution oracle: `ci/benchmark_runner.py`
- Platform workflows: `.github/workflows/ci.yml`, `.github/workflows/linker-modes.yml`, and
  `.github/workflows/benchmark-stats.yml`

When native Mach-O code generation lands, update this guide in the same change to name its pinned
behavioral baseline and native structural/differential test suite.
