# mimalloc-pprof provenance and update procedure

`reld` consumes the MIT-licensed [`mimalloc-pprof`](https://crates.io/crates/mimalloc-pprof)
crate at the exact version recorded in the workspace manifest. The lockfile records the
registry checksum. This direct dependency and its resolved graph were explicitly approved
in #97 under the dependency policy tracked by #88.

## Configurations

- Default Linux build: `mimalloc-pprof` is the sole global allocator and sampled
  profiler hooks are compiled out (`MI_PPROF=0`). Internal exact DHAT is inactive.
- Sampled diagnostic build: `--features mimalloc-pprof-profile` compiles pprof hooks
  in; collection remains explicitly runtime-controlled.
- Exact diagnostic build: `--features mimalloc-pprof-dhat` uses mimalloc-pprof's
  internal DHAT collector and writes `dhat-heap.json` (override with
  `RELD_DHAT_OUTPUT`). It does not replace the global allocator.
- There is no supported system-allocator configuration. The system allocator is
  retained only as the pinned pre-change baseline for artifact comparison.

Authoritative timing uses only the default hook-free build and must prove sampled
pprof and exact DHAT are runtime-off. Sampled and exact profile-collection runs are
diagnostic evidence, never timing evidence.

## Updating

1. Review the intended published crate, license, feature contract, and complete
   resolved dependency graph.
2. Obtain explicit developer approval for any new direct or transitive identity.
3. Change the exact workspace version and refresh the lockfile with the pinned Rust
   1.95 toolchain; verify the registry checksum rather than substituting a path or git source.
4. Run the Linux validation commands documented in `agents/platforms/linux.md`.
   This allocator-only change must retain raw artifact identity and exact native
   execution output before any performance measurement is reported.
