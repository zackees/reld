# mimalloc-pprof provenance and update procedure

`reld` consumes the MIT-licensed [`mimalloc-pprof`](https://crates.io/crates/mimalloc-pprof)
crate at the exact version recorded in the workspace manifest. The lockfile records the
registry checksum. This direct dependency and its resolved graph were explicitly approved
in #97 under the dependency policy tracked by #88.

## Configurations

- Default Linux build: Rust's system allocator. The optional `mimalloc-pprof`
  dependency and its profiling hooks are not compiled into this configuration.
- Sampled diagnostic build: `--features mimalloc-pprof-profile` compiles pprof hooks
  in and selects mimalloc as the sole global allocator; collection remains explicitly
  runtime-controlled.
- Exact diagnostic build: `--features mimalloc-pprof-dhat` uses mimalloc-pprof's
  internal DHAT collector and writes `dhat-heap.json` (override with
  `RELD_DHAT_OUTPUT`). It uses that same mimalloc global allocator and never installs
  a second allocator.

The #93 allocator benchmark measured 0.000% aggregate wall improvement with a paired
bootstrap 95% confidence interval of -1.610% to +0.385%. Its CPU interval was also
inconclusive, so the #97 decision gate returned `keep_allocator=false` and restored
the system allocator as the production default. Sampled and exact profile-collection
runs remain diagnostic evidence, never production timing evidence.

## Updating

1. Review the intended published crate, license, feature contract, and complete
   resolved dependency graph.
2. Obtain explicit developer approval for any new direct or transitive identity.
3. Change the exact workspace version and refresh the lockfile with the pinned Rust
   1.95 toolchain; verify the registry checksum rather than substituting a path or git source.
4. Run the Linux validation commands documented in `agents/platforms/linux.md`.
   This allocator-only change must retain raw artifact identity and exact native
   execution output before any performance measurement is reported.
