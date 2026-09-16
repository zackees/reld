# Routing maintenance: keep the fast engine honest when the native linker changes

Companion to [`polylinker.md`](polylinker.md) (the model) and
[#123](https://github.com/zackees/reld/issues/123) (the audit, phased design, and testing
criteria). This note is procedural: what an agent must do to routing whenever it touches the
native engine, adds a flag, or adds an engine, and how a routing decision is surfaced.

**The invariant.** reld is a polylinker: every link request is either honored by the native
engine, honored by a bundled engine reld routes to, or rejected loudly. A native change that
does not update routing breaks that invariant in one of two silent ways:

- the native engine learned a capability, but links still leave it (a perf regression nobody sees);
- the native engine lost or never had a capability, but links still stay (a silent mislink).

A PR that enhances the native engine and does not touch routing is incomplete. Reviewers reject it.

## Where routing lives today

| Concern | Location |
|---|---|
| Engines and their capability sets | `crates/reld-core/src/bridge.rs`: `Capability`, `NATIVE_RELD_CAPABILITIES`, `LLD_CAPABILITIES`, `ENGINES` |
| Flag → capability classification (runs before native parsing, incl. `@response` files) | `bridge.rs::collect_requested_capabilities` |
| Engine selection, overrides, fallback ordering | `bridge.rs::select_engine`, `select_route_with_policy` |
| What is stripped before forwarding to a child linker | `bridge.rs::forwarded_args`, `forwarded_args_for_engine` |
| Native flag tables | `crates/reld-core/src/args/elf.rs`: `SILENTLY_IGNORED_FLAGS`, `IGNORED_FLAGS`, `DEFAULT_FLAGS`, the `-z` sub-option table, `warn_unsupported` value fallbacks |
| Native conformance evidence | `crates/reld/tests/external_tests/mold_skip_tests.toml` (mold suite ratchet, #14), `crates/reld/tests/acceptance.rs`, `reld-difftest` |
| Route expectations in CI | `ci/consumer_acceptance.py` (`expected_route`), `crates/reld/tests/nix_rpath.rs` |
| Human-facing capability claims | `README.md` "Polylinker" table, `agents/docs/polylinker.md`, `DESIGN.md` §4.5 |

#123 replaces the scattered tables with one `FlagRule` table carrying a four-way `Disposition`
(`Native`, `SatisfiedByConstruction`, `Requires(Capability)`, `Unsupported`) and generated
`docs/flags.md`. Until that lands, treat the locations above as the table and keep them consistent
by hand. Once it lands, the table is the only place a disposition may be declared, and the
procedures below become edits to one file plus tests.

## Promoting a capability: making the fast engine handle what lld used to

Do this when you implement natively something that currently routes to `lld` (for example
`--no-undefined-version`, `-z text`, `SHF_GNU_RETAIN`, `.deplibs`).

1. **Prove before you promote.** Land the native implementation with its conformance test first,
   while the flag still routes to `lld`. The test must exercise the native engine explicitly
   (`--engine=reld` today; `RELD_REQUIRE_ENGINE=reld` once #123 Phase 5 lands) so it does not pass
   by accident through the bridge. If a mold-suite test covers the flag, remove it from
   `mold_skip_tests.toml` in the same PR; a promoted flag may not keep a skip entry.
2. **Flip the disposition.** Remove the flag's arm from `collect_requested_capabilities` (or change
   its `FlagRule` from `Requires(...)` to `Native`). If the `Capability` variant has no remaining
   triggers, delete the variant and its `LLD_CAPABILITIES` entry so the table cannot advertise a
   route that nothing uses.
3. **Update the routing tests.** `bridge.rs` has tests asserting the flag routes to `lld`
   (`unsupported_native_semantics_route_to_lld`, `elf_lto_spellings_route_to_lld`, …). Invert
   them: the flag now stays native. Add the flag to a "stays native" list rather than
   deleting the assertion; a deleted assertion is how a regression becomes invisible.
4. **Update route expectations downstream.** Any corpus row or consumer-acceptance project whose
   expected route was `lld` because of this flag flips to `reld`. If rustc emits the flag on
   ordinary links (as with `--no-undefined-version` on every proc-macro), add a
   `RELD_REQUIRE_ENGINE=reld` row for that rustc configuration so the promotion is pinned.
5. **Check satisfied-by-construction neighbors.** A native implementation often changes a default
   (for example implementing `-z text` changes what `-z notext` means). Every
   `SatisfiedByConstruction` rule that names the same property needs its equivalence test re-run
   and possibly re-classified.
6. **Regenerate the claims.** `README.md` polylinker table, `polylinker.md` "shipped vs designed",
   `DESIGN.md` §4.5 capability list, and `docs/flags.md` once it exists. Do not describe the
   capability as native anywhere before steps 1–4 are merged.
7. **Benchmarks.** If the flag appears in a benchmarked configuration, the benchmark's recorded
   `engine` field changes from `lld` to `reld`. Say so in the PR; the benchmark gate treats an
   unexplained engine change as a coverage error (#63).

## Demoting a capability: the native engine is wrong

Do this the moment a native bug is found for a flag currently marked native, before the fix.

1. Add a `Requires(lld)` classification for the flag (an arm in `collect_requested_capabilities`
   plus a `Capability` that `lld` measurably has; check lld's option table first, see "Adding a
   flag" below). This ships the correct behavior immediately at lld speed.
2. Add the failing case to `mold_skip_tests.toml` or `acceptance.rs` with the tracking issue.
3. Fix natively, then follow the promotion procedure.

Never leave a native bug as a warning, a `SILENTLY_IGNORED_*` entry, or a `warn_unsupported`
value fallback. Those are the silent-drop paths #121 and #123 exist to remove.

## Adding a flag the native engine does not implement

Decide its disposition with this order of questions; write the answer down in the rule.

1. **Does the native engine's unconditional behavior already imply it?** (`--start-group`,
   `-z now`, `--nostdlib`.) Then `SatisfiedByConstruction`, with an equivalence test comparing
   the governed property against `ld.lld`. Do not route it.
2. **Does a bundled engine actually honor it?** Check the engine's option table, not just whether
   it accepts the flag. `ld.lld --help` marks `--start-group`, `--sort-common`, `--rpath-link`
   "Ignored for compatibility"; `lld-link` and `ld64.lld` accept dozens of flags they ignore, with
   no diagnostic. Routing to an engine that ignores the flag is not escalation. If lld honors it:
   `Requires(Capability)` with `Measured { probe }` provenance.
3. **Does no bundled engine honor it?** `Unsupported`: a loud error naming the flag and the
   missing engine. `RELD_UNSUPPORTED=ignore` is the only escape hatch.
4. **Is it reld-only?** (`--time`, `--nix-rpath`, `--fork`, `--write-layout`.) Either
   `NativeControl` (forces the native engine; conflicts loudly with a routed requirement) or
   stripped before forwarding with a `RELD_LOG_ENGINE` note. It must never reach a child linker.

Spellings are case-sensitive for GNU and ld64 flags and case-insensitive for COFF. Lowercasing
GNU flags is how `-X` was once routed as `-x`.

Semantics carried by inputs rather than flags (`SHF_GNU_RETAIN`, `.deplibs`, bitcode magic,
`e_machine` outside the native set, linker-script commands outside the native grammar) get rules
too. The classifier probes inputs before the native parser sees them (#123 D2).

## Adding an engine

Add it behind the capability table: declare its format, its `Measured`/`Documented`
capabilities, and its discovery. Do not add a dispatch branch at a call site, and do not copy
another engine's capability list (`COFF_LLD_ENGINE` and `MACHO_LLD_ENGINE` currently share the
ELF-shaped `LLD_CAPABILITIES`; that is a bug to fix, not a pattern). Anything reld adds on top
of linking (the Nix RUNPATH derivation, identity comments) must be applied as an argv-level
transform before engine selection so every engine sees the same request (#123 D6).

## How a routing decision is surfaced, and why stderr is not free

**Channels, in order of preference:**

| Channel | When | Cost |
|---|---|---|
| `RELD_INVOCATION_LOG=<path>` (JSONL, one record per successful link: engine, `route_kind`, reason, output, argv) | acceptance tests, benchmark harnesses, audits | none on stderr |
| `.comment` section of the output | always, post hoc | none; `readelf -p .comment out` shows `Linker: reld …` for native or `Linker: LLD …` for a bridged link |
| `RELD_LOG_ENGINE=1` → one stderr line `reld: engine=<name> (<native|bridge>, reason=<…>)` | interactive debugging, CI logs | see below |
| `RELD_REQUIRE_ENGINE=<name>` (#123 Phase 5) → hard error if routing picks anything else | CI rows that must pin a route | fails the build, by design |

**Why the default is silent.** Linkers are run by compiler drivers, and drivers own stderr.
Measured with rustc 1.95:

- rustc hides linker stderr on a successful link by default; the `linker_messages` lint is
  allow-by-default.
- With `-W linker-messages` (or a crate-level `#![warn(linker_messages)]`) every byte a linker
  writes to stderr becomes `warning: linker stderr: …` on every link.
- With `-D linker-messages`, that same byte is `error: linker stderr: …` and the build fails.
  `-D warnings` alone does not promote it, but projects that enable the lint and deny warnings
  are common in CI.
- clang and gcc pass linker stderr through untouched; make and cmake do not fail on it, but they
  print it for every link.
- A build that sets `--fatal-warnings` is unaffected: that flag governs the linker's own
  diagnostics, and lld gets it forwarded, so a bridged link that prints warnings will fail there.

So: an unconditional stderr line on success is a correctness hazard for users with the lint
denied and noise for everyone else. Never write to stderr on a successful link unless
`RELD_LOG_ENGINE` is set. Never print "falling back to lld" as a warning: a fallback that is by
design is not a warning, and one that is not by design must be a hard error. The way to make a
routing regression visible is not a message a human might read; it is a `RELD_REQUIRE_ENGINE`
row in CI that fails, and the `.comment`/invocation-log record that an audit can query.

Failures are different: a bridged child that exits non-zero has already written its own
diagnostics, and reld exits with the child's status. When the child dies by signal, reld must say
so on stderr (name the engine and the signal) rather than exit 1 silently.

## PR checklist for any change under `crates/reld-core/src/{args,bridge,elf*,layout*}` or the `-z` table

- [ ] Every flag, `-z` keyword, input property, or default I touched has a disposition, and I
      can name it.
- [ ] No new `|_, _| Ok(())` handler, `SILENTLY_IGNORED_*` entry, or `warn_unsupported` fallback.
- [ ] If I promoted: native conformance test merged first, mold skip entry removed, routing test
      inverted, corpus/consumer route expectation flipped, README/polylinker/DESIGN regenerated.
- [ ] If I demoted or added `Requires(lld)`: I checked lld's option table shows the flag is
      honored, not "ignored for compatibility".
- [ ] If I changed a default (GC, RPATH tag, execstack, hash style, TEXTREL policy): deviation
      register entry and an equivalence test.
- [ ] Nothing new writes to stderr on a successful link without `RELD_LOG_ENGINE`.
- [ ] Reld-only flags I added are `NativeControl` or stripped; none can reach a child linker.
- [ ] `RELD_LOG_ENGINE=1` output for the affected configuration is pasted in the PR.
