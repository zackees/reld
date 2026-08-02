# Phase 1 acceptance harness

This document records the reproducible inputs behind issue #5. The normative requirements remain
in `docs/plan/03-PHASE-1-HARNESS.md`.

## Frozen fixture language

`crates/reld/tests/acceptance.rs` accepts the 30 directives listed in the plan plus its explicit
`Requires*` capability family. Unknown directives and the deprecated `SkipLinker` / `EnableLinker`
spellings are errors with a source file and line. The harness audits every inherited fixture before
host/platform filtering, automatically admits fixtures expressed entirely in the frozen language,
and explicitly defers fixtures that use a recognized legacy directive. Phase 2 owns migration of
that accounted deferred set.

## Mold provenance

The mold submodule is pinned to
`9b101aa9c75aee583e9a6dfc99e88c43a8f6d16a` (2026-08-01, merge of mold PR #1629). This is the
earliest first-parent mold revision whose tree contains exactly:

- 518 `test/*.sh` files total;
- 111 `arch-*` tests, excluded from Phase 1 because they require the deferred cross/qemu matrix;
- 407 native tests admitted to the ratchet.

The harness asserts all three counts before registering trials. Unsupported-by-reld skips are
expect-failure trials tracked by #14. A mold test that exits successfully because a runtime
prerequisite is missing is reported as skipped, not mistaken for a ratchet failure.

## Oracle policy

The ELF differ reports relocation coverage and enforces a 50% floor. Non-ELF structural dispatch
fails loudly until its backend phase supplies an implementation. Inherited default ignores are
typed as tracked by #13; fixture-specific ignores use the no-longer-needed ratchet.

## Native CI references

The four jobs publish exact executed/skipped counts and the tool-reported versions in the Actions
summary. The intended reference set is:

| Job | Native runner | References |
|---|---|---|
| linux-gnu x86_64 | `ubuntu-24.04` | GNU binutils 2.42, LLVM lld 18.1.8, mold 2.41.0, wild 0.9.0 |
| windows-gnu x86_64 | `windows-2022`, MSYS2 UCRT64 | the recorded UCRT64 binutils package, LLVM lld 18.1.8 |
| windows-msvc x86_64 | `windows-2022` | version-gated MSVC 14.44 `link.exe`, LLVM `lld-link` 18.1.8 |
| macOS arm64 | `macos-14` | version-gated Apple ld 1115.7.3, Homebrew LLVM 18.1.8 `ld64.lld` |

Runner-provided proprietary/system linkers are checked against committed expected version strings;
downloaded open-source linkers use explicit releases and checksums. Every matrix entry records the
actual versions and fails on drift, so a weekly runner-image update cannot be silent. The four
native targets share one matrix; adding another target is one matrix entry plus any genuinely new
platform setup it requires.
