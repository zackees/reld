# Upstream tracking

reld is a fork of [wild](https://github.com/wild-linker/wild).

- Vendored commit: `5793935f1d8b05b9a978ce2089e16e718072e9a9` (`wild-linker` v0.9.0)
- Vendor date: 2026-08-01
- License: MIT OR Apache-2.0

## Intentional divergences

- The six upstream crates live under `crates/` and use the `reld-*` package and Rust identifiers.
- User-facing linker names and `WILD_*` environment variables use `reld` and `RELD_*`.
- The experimental WebAssembly backend, dependencies, fixtures, and CI jobs were removed.
- `PlatformKind::Coff` and the `reld-link` dispatch point exist, but linking bails with the Phase 3
  diagnostic; host selection remains unchanged.
- `ld.reld`, `reld-link`, and `ld64.reld` are materialized as portable driver shims rather than Git
  symlinks so Windows checkouts work.
- reld retains its own testkit, benchmark workflows, project documentation, and roadmap.
- wild's pinned mold and binary-fixture submodules remain pinned as Git submodules.

Run `bash ci/upstream-diff.sh --stat` to see the current source delta. Pass a commit SHA after
`--stat` (or by itself for a full diff) to compare against a different upstream revision.
