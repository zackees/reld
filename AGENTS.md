# Contributor agent routing

`DESIGN.md` is the project contract. Before changing linker behavior, output writing, correctness
tests, benchmarks, or platform CI, read its §3.1 artifact-equivalence policy.

Platform execution details are intentionally owned by separate guides:

- Linux/ELF: [`agents/platforms/linux.md`](agents/platforms/linux.md)
- Windows/PE-COFF: [`agents/platforms/windows.md`](agents/platforms/windows.md)
- macOS/Mach-O: [`agents/platforms/macos.md`](agents/platforms/macos.md)

Read the guide for every affected target before editing. A shared change in `crates/reld-core`,
`ci/`, or `.github/workflows` may affect all three targets and therefore requires all three guides.
Keep platform-specific commands, reference tools, deterministic-field handling, and acceptance
evidence in the platform guide rather than growing this root file into a centralized runbook.

Across every platform:

- Performance-only changes require artifact comparison before performance claims.
- Prefer raw byte identity. Any normalization must name the exact nondeterministic field and retain
  comparison of every other byte.
- Native execution with exact observable output is mandatory but is only one correctness layer.
- Pin reference linker revisions and toolchains. Do not silently test against a floating install.
- Intentional artifact changes must be declared and structurally tested.
- Update the relevant platform guide when a platform's engine, reference linker, artifact format,
  or validation command changes.
- Routing is part of every native-engine change. Enhancing, fixing, or removing a native
  capability requires promoting or demoting its routing entry, flipping the routing tests and CI
  route expectations, and regenerating the capability claims, in the same PR. Follow
  [`agents/docs/routing-maintenance.md`](agents/docs/routing-maintenance.md); the model is in
  [`agents/docs/polylinker.md`](agents/docs/polylinker.md) and the audit in
  [#123](https://github.com/zackees/reld/issues/123). Never write to stderr on a successful link
  without `RELD_LOG_ENGINE`.
- Host-platform selection (`windows`, `unix`, `target_os`, `target_env`, ... in `cfg`/`cfg!`/
  `cfg_select!`) and native OS APIs (`std::os::*`, `libc`, `windows_sys`) belong only in
  `crates/reld-core/src/platforms/mod.rs` and its `platform_<tree>` concrete trees. Everything else,
  tests included, uses `reld_core::platforms::{fs,host,linker_plugin,path,process}`; `target_arch`
  and `target_endian` stay allowed. `crate::platform` is the unrelated linker-format trait. The
  `ban_platform_cfg_outside_boundary` Dylint enforces this with no baseline; see
  [`dylints/README.md`](dylints/README.md).

## Dependency approval

The linker's existing dependency graph is a fixed budget. Do not add a direct, development,
build, target-specific, feature-gated, or transitive crate without explicit developer approval
obtained before the dependency is added. An agent cannot grant that approval, infer it from a
feature request, or update a dependency baseline to approve its own change. Implement logging and
other support features with the standard library or already-approved crates.

When a dependency check fails, preserve this actionable guidance in its diagnostic:

> Adding crates to the reld linker requires explicit developer approval. Agents must not update
> the dependency baseline to bypass this check. Use the standard library or an already-approved
> dependency, or obtain developer approval and update the baseline in the same reviewed change.

Automated enforcement is tracked by [issue #88](https://github.com/zackees/reld/issues/88).

## Crate layout

The workspace is a fixed set of crates, like the dependency graph above. Add a module, not a
crate. `reld-core` being large is not a reason to split it: it is one program, and the crate
boundary is not what its compile time is made of.

A new workspace member needs explicit developer approval, requested in the issue that motivates
it, before the work starts. State which of these it meets, all of which must hold:

1. More than one crate depends on it, or something outside this repository consumes it. A single
   consumer is a module.
2. It removes a dependency edge that would otherwise be wrong, e.g. two crates needing shared
   types without depending on each other. `reld-reloc`, `reld-layout-schema` and `reld-trace`
   exist for exactly this.
3. Its public surface is narrow and stable enough to describe in a sentence.
4. It creates no cycle and no diamond that forces a lockstep bump across the workspace.
5. It is not test-only scaffolding, unless several test targets share it, in which case it is
   `publish = false` like `reld-testkit`.

A long file, a feeling that something is its own concern, an unmeasured hope of faster
incremental builds, and mirroring another project's layout are all insufficient on their own.
Never work around a missing crate by duplicating code across crates: say the boundary hurts and
ask.

Every crate is `publish = false`. reld ships as per-platform binaries from GitHub Releases
([#148](https://github.com/zackees/reld/issues/148)), and none of these crates is optional
functionality for an outside consumer. Publishing to crates.io is a product decision for the
owner, tracked in [issue #152](https://github.com/zackees/reld/issues/152), not something to
enable in passing.

`ci/tests/test_crate_layout.py` pins both the member list and `publish = false`, so a new crate
cannot arrive without a reviewed change that says so.

## Test layout

The number of test files and test targets is itself a budget, for the same reason the crate
count is: every `[[test]]` target is a separate link, every new `ci/tests/test_*.py` is another
place a future reader has to look, and a suite nobody can hold in their head stops being read
and starts being skipped.

Put a new case in the file that already covers its subject. A new file is warranted when the
subject genuinely has no home, not when the existing file is long. Before adding a new
`[[test]]` target in a `Cargo.toml` or a new top-level suite, ask the developer in the issue that
motivates it and say which existing target you considered and why it does not fit.

A test earns its place by pinning a property that can actually regress:

1. It can fail. If no plausible change makes it red, it is documentation written in an expensive
   format — delete it or turn it into a comment.
2. It states the property, not the implementation. Asserting that a generator's output matches
   the generator is a tautology: both sides move together and the test passes through the
   regression it was meant to catch.
3. It fails with the cause, not just the fact. Name the target, the offset, the flag — whatever
   the next person needs in order to start.
4. It is not a near-duplicate of its neighbours. Three cases that differ only in a constant are
   one table-driven case.
5. Its cost matches its evidence. A gate that takes gigabytes or minutes runs on a schedule and
   keeps its own logic unit tested per PR, rather than slowing every push.

Deleting a test that no longer pins anything is maintenance, not a gap. Say so in the PR, and
name what still covers the property if anything does.
