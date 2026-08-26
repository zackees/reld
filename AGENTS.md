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
