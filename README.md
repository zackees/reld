# reld

*relink. reweld. reload.*

**A fork of the [`wild`](https://github.com/wild-linker/wild) linker** — with Windows,
Linux, and macOS as co-equal targets, incremental linking as the architecture, and a mandate to
beat `wild` in every measured category.

`reld` is a **polylinker**: it puts multiple real linker engines behind a single dispatch point,
so it can run everywhere and route a request to whichever engine supports it — fast by default,
escalating only when the flags demand it. See
["Polylinker" below](#polylinker-runs-everywhere-supports-everything-by-routing) for what that
means concretely, and what part of it is shipped vs. designed.

> **Status: reld links real programs on all three platforms today.** Linux/ELF links natively
> (inherited from wild). Windows/COFF and macOS/Mach-O link via a **bridge** to `lld` (the
> `rust-lld` that ships with every Rust toolchain) — proven by CI end-to-end builds on the
> windows-msvc and macos-arm64 runners. Native COFF/Mach-O codegen in reld's own engine is still
> **future work**. Linux also routes LTO/plugin, ICF, and several compatibility-policy flags to
> the ELF `lld` bridge when the native engine cannot honor them. Every decision is logged with
> the selected engine and reason. See [#30](https://github.com/zackees/reld/issues/30) and
> decision B8 in [#17](https://github.com/zackees/reld/issues/17). Every performance number below remains a
> **target**, not a measurement, except where noted. See [DESIGN.md](DESIGN.md) and
> [#17](https://github.com/zackees/reld/issues/17) for the phase history.

<!-- BENCHMARK:BEGIN -->
<p align="center"><b>x86_64-linux</b><br>
<a href="https://github.com/zackees/reld/tree/benchmark-stats/x86_64-linux"><img alt="Linux reld link benchmark" src="https://raw.githubusercontent.com/zackees/reld/benchmark-stats/x86_64-linux/benchmark-link.jpg" width="100%"></a></p>

<p align="center"><b>x86_64-pc-windows-msvc</b><br>
<a href="https://github.com/zackees/reld/tree/benchmark-stats/x86_64-pc-windows-msvc"><img alt="Windows reld link benchmark" src="https://raw.githubusercontent.com/zackees/reld/benchmark-stats/x86_64-pc-windows-msvc/benchmark-link.jpg" width="100%"></a></p>

<p align="center"><b>aarch64-apple-darwin</b><br>
<a href="https://github.com/zackees/reld/tree/benchmark-stats/aarch64-apple-darwin"><img alt="macOS reld link benchmark" src="https://raw.githubusercontent.com/zackees/reld/benchmark-stats/aarch64-apple-darwin/benchmark-link.jpg" width="100%"></a></p>

*Auto-generated nightly by [`benchmark-stats.yml`](.github/workflows/benchmark-stats.yml) and
published to the [`benchmark-stats` branch](https://github.com/zackees/reld/tree/benchmark-stats),
with independent `latest.json` and `history.jsonl` per target. Linux measures reld's native
engine; Windows and macOS mark reld **`pending`** (unsupported-by-design in this clang-based
harness) until the rustc-based bridge measurement lands — an explicit, documented state that
`latest.json` and the charts render distinctly from a failed `n/a`. Each platform gates its
**expected** linkers in CI (Linux `bfd`/`lld`/`mold`/`wild`/`reld`, Windows `link.exe`/`lld`,
macOS `ld`/`ld64.lld`): a missing timing for an expected linker fails the build, so coverage can
never silently understate itself (see [#63](https://github.com/zackees/reld/issues/63)). Every
chart labels reference, native, and bridge series modes in its generated metadata.*
<!-- BENCHMARK:END -->

## What it is

`reld` is a source fork of `wild` at commit
`5793935f1d8b05b9a978ce2089e16e718072e9a9`. The inherited ELF linker works now, and a bridge to
`lld` makes Windows and macOS link today too; the scope then expands toward native Windows and
macOS backends (reld's own COFF/Mach-O codegen) and incremental linking:

| | `wild` | `reld` |
|---|---|---|
| Platforms | Linux / ELF | **Linux / ELF native; Windows / macOS via lld bridge — all three work today** |
| Incremental linking | on the roadmap | **the architecture** |
| Optimizes for | the final link | the **edit → link → run** loop |
| Release-quality output | required | **explicit non-goal** |

### Bridge status

| Platform | Format | Today | Native backend |
|---|---|---|---|
| Linux | ELF | **Native** — inherited wild core | Already native |
| Windows | PE/COFF | **Bridge** — delegates to `lld-link` (`rust-lld`) | Future (issue #7) |
| macOS | Mach-O | **Bridge** — delegates to `ld64.lld` (`rust-lld`) | Future (issue #8) |

The bridge is a real, CI-proven delegation to the `lld` shipped with every Rust toolchain, not a
stub — it links and runs a multi-crate, C-dependency (`rusqlite` bundled/SQLite) program on both
platforms today. It is not reld's own codegen; see
[issue #17](https://github.com/zackees/reld/issues/17) for the full BR-1…BR-4 phase history and
[DESIGN.md](DESIGN.md) for the architecture.

## Polylinker: runs everywhere, supports everything, by routing

`reld` bundles more than one real linker per platform — today, its own native engine (Linux/ELF)
and the `lld` bridge (Windows/COFF, macOS/Mach-O). That makes it a **polylinker**: a single
binary that can, in principle, satisfy any requested link configuration by routing the request to
whichever bundled engine already supports it, rather than by implementing every feature natively.
Concretely (per [#30](https://github.com/zackees/reld/issues/30)):

- **Fast by default.** Ordinary dev-loop links use the fastest engine available for the platform.
- **Escalate on demand.** A configuration the fast engine can't do — the leading example is
  **LTO** — gets routed to a bundled engine that *can* (e.g. `lld`'s real LTO/plugin support),
  instead of being rejected.
- **Fall back, never mislink.** If the fastest engine lacks a requested capability, reld falls
  back to a capable bundled engine. If *no* bundled engine supports the configuration, that's a
  loud, specific error — never a silent mislink.
- **Explicit override.** `--engine=<name>` / `RELD_ENGINE` will force a specific bundled engine.
- **Always observable.** Every routing decision is meant to be logged, so a user can always tell
  which real linker ran and why.

**What's shipped today vs. designed:**

| | Status |
|---|---|
| Bundling multiple real linkers per platform | **Shipped** — native ELF engine + lld bridge, see "Bridge status" above |
| Routing by platform/format (dispatch to native vs. bridge) | **Shipped** |
| Routing by requested *flags/config* | **Shipped for ELF** — LTO/plugin, ICF, discard-all, warning/color policy, version-script policy, and Cortex-A53 erratum 843419 route to `lld` |
| Capability table per bundled engine | **Shipped, initial set** — extended as more native gaps are identified |
| Fallback ordering when the fast engine lacks a capability | **Shipped** — native ELF first, capable ELF `lld` fallback |
| `--engine=` / `RELD_ENGINE` override | **Shipped** — `reld` or `lld` on ELF; format-specific lld drivers elsewhere |
| Per-decision routing log line | **Shipped** |

An ELF LTO link is automatically routed to `lld` when reld sees `-flto` or a linker plugin
request, including plugin options found in response files. See [DESIGN.md](DESIGN.md) for the full
routing design and [`agents/docs/polylinker.md`](agents/docs/polylinker.md) for the contributor-
facing summary.

## The four claims

1. **Runs and targets everywhere** — Windows, Linux, macOS. Not "Linux plus ports." The
   polylinker framing extends this claim toward "and supports every requested link
   configuration," via routing — see above for the capability set shipped today.
2. **Incremental** — a one-object change reflows the image instead of rebuilding it.
   Target: **warm relink in low single-digit milliseconds**, largely independent of binary size.
3. **Faster than `wild` in every category** — cold link, warm full link, warm incremental,
   peak RSS, *and* single-threaded. All five, or the claim fails. See
   [DESIGN.md §2.3](DESIGN.md) for why single-threaded is on that list.
4. **Benchmarks are auto-generated and published by CI** — same harness pattern as
   [`zccache`](https://github.com/zackees/zccache), on pinned competitor versions, with
   regressions published rather than hidden.

## Why this is worth doing

Dropping release linking as a goal is not a limitation to apologize for — it is the design
freedom that makes the rest tractable. LTO deferred, no PGO/BOLT interaction, no
reproducible-build determinism, and a correctness bar of "correct on this developer's machine
right now" instead
of "byte-identical and correct for every downstream consumer forever." That bar is what has
historically consumed years and killed prior efforts outright.

And the evidence says throughput is the wrong thing to chase. On a 46 MB debug Rust binary on
`x86_64-pc-windows-gnu`, `ld.bfd` and `ld.lld` measure **statistically identical**. If a
target's entire link budget is ~1 second, an infinitely fast batch linker saves under a second.
**Incremental is the product; throughput is table stakes.**

Notably, incremental linking sits **above** both Mach-O and Windows on `wild`'s own roadmap —
the clearest signal available that this is the unserved need.

## Non-goals

- Release / shipping builds
- Byte-for-byte reproducible output
- Drop-in `ld` replacement for distro build systems

Use `lld`, `wild`, or your platform linker for those. `reld` is for the 200th rebuild of the
day, not the one that goes to customers.

## Stretch goals

**(Thin)LTO is a stretch goal, not a non-goal** — deliberately sequenced after the fast linker
on all three platforms. The inner loop is the priority; LTO touches nearly every subsystem and
would slow down the thing this project exists to deliver.

The product-level LTO path is shipped: `-flto`/`--plugin`/`/LTCG` requests select a bundled
`lld` driver with real LTO support (the polylinker model above), while reld's native engine owns
zero LTO codegen. Implementing native LTO remains the stretch goal. An explicit
`--engine=reld` on an LTO request fails with a capability-specific diagnostic rather than
silently mislinking; see [D13](docs/plan/01-DECISIONS.md).

## Honest risks

Recorded up front rather than discovered later — see [DESIGN.md §5](DESIGN.md):

- **macOS is weak on speed.** Apple's `ld_prime` is already **1.4–2× faster than `ld64.lld`**
  (measured by `wild`'s own Mach-O lead). The honest macOS pitch is incremental linking and an
  open implementation, not raw throughput.
- **~1.5–2 person-years per format backend** to production, by expert practitioners.
- **Unification has failed before.** LLD began as a unified multi-format linker and deleted
  that abstraction in 2021 — 244 files, 36,441 lines. Its backends now share ~2.5% of their
  code. `reld` bets that Rust's monomorphized traits change this calculus, and scopes the
  abstraction narrowly on that basis.

## Development and acceptance

P0–P2 acceptance runs inside the vendored Linux development container, not directly on the
Windows host:

```bash
docker build -t reld-ci -f docker/ci/ubuntu.Dockerfile .
docker run --rm -v "$PWD:/src" -w /src reld-ci \
  bash -lc 'RELD_TEST_CONFIG=/src/test-config-ci.toml rustup run --install 1.94.1 cargo test --workspace'
```

See [UPSTREAM.md](UPSTREAM.md) for the tracked source delta.

## License

The code derived from wild is dual-licensed MIT OR Apache-2.0; see [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [NOTICE](NOTICE). The repository's original reld components
remain BSD 3-Clause under [LICENSE](LICENSE).

## Related

- [`soldr`](https://github.com/zackees/soldr) — build tool with content-addressed caching.
  `soldr` makes the compile fast; `reld` is the other half of the loop.
