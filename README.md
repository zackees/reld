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
> the ELF `lld` bridge when the native engine cannot honor them. Set `RELD_LOG_ENGINE=1` to log
> each selected engine and reason. See [#30](https://github.com/zackees/reld/issues/30) and
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
with independent `latest.json` and `history.jsonl` per target. Each chart links the same
idiomatic, moderately link-heavy Rust artifact-auditing project in `no-LTO`, `ThinLTO`, and `full-LTO`
configurations; compilation happens once per configuration and only the captured final link
is timed. Fixed linker startup is measured and reported separately, never subtracted, and a
10% significance gate prevents startup-dominated results. Linux measures reld's native
engine; Windows and macOS measure reld through their target-correct `lld` **bridge** front doors.
`latest.json` records both the series `mode` (`native` or `bridge`) and concrete `engine` (`reld`
on Linux, `lld-link` on Windows, `ld64.lld` on macOS), and the charts label bridge results so they
are never presented as native COFF/Mach-O throughput. Each platform gates its **expected** linkers
in CI (Linux `bfd`/`lld`/`mold`/`wild`/`reld`, Windows `link.exe`/`lld`/`reld`, macOS
`ld`/`ld64.lld`/`reld`): a missing timing or a missing/duplicate/unexpected configuration
fails the build, so coverage can never silently understate itself (see
[#63](https://github.com/zackees/reld/issues/63)). The generated artifact freshness guard also
requires every published chart to name the source SHA and current generation time.*
<!-- BENCHMARK:END -->

## Linux linker competition: final-link time and peak RSS

<p align="center">
<a href="https://github.com/zackees/reld/releases/tag/linux-linker-competition-v2"><img alt="Grouped dual-axis Linux ELF linker competition: each linker has a solid wall-time bar first on the left seconds axis and a hatched peak-RSS bar second on the right MiB axis" src="https://github.com/zackees/reld/releases/download/linux-linker-competition-v2/competition.png" width="100%"></a>
</p>

This is the competitive Linux measurement, separate from the cross-platform trend charts above.
It links a frozen LLVM/Clang C++ corpus that was compiled in advance, so **compilation is excluded**
and only the final linker invocation is timed. Bars are medians from 12 measured trials in two
balanced six-linker Williams blocks after two warmups; whiskers are bootstrap 95% confidence
intervals. Within each linker group, the solid wall-time bar comes first and uses the zero-based
left seconds axis; the adjacent hatched peak-RSS bar comes second and uses the independent
zero-based right MiB axis. Peak RSS is the maximum summed resident memory of the linker process
tree, sampled in a fresh cgroup-v2 subtree for every trial. The metrics are not normalized or
combined into one score.

| Linker | Median link time | Median peak RSS |
|---|---:|---:|
| GNU bfd | 3.643 s | 894.660 MiB |
| LLD | 0.417 s | 753.797 MiB |
| mold | 0.295 s | 760.838 MiB |
| Wild | 0.262 s | 529.902 MiB |
| reld baseline | 0.247 s | 530.152 MiB |
| **reld candidate** | **0.250 s** | **530.344 MiB** |

On this workload, the paired 95% intervals show reld reducing link time and peak RSS versus GNU
bfd (93.0–93.2% and 40.6–40.8%), LLD (38.7–42.1% and 29.5–29.8%), and mold (13.3–16.8% and
30.1–30.5%). Against Wild, reld is 2.2–5.6% faster on wall time but statistically tied on RSS;
against the exact pre-change reld baseline, both intervals cross zero. So this evidence supports
clear two-metric wins over bfd, LLD, and mold, but **does not support claiming that reld is way
better than Wild or that this candidate is a performance breakthrough over its baseline**.

The [hosted run](https://github.com/zackees/reld/actions/runs/33496554602) passed output identity,
external self-determinism, native execution, and live RSS-calibration gates. The permanent
[evidence release](https://github.com/zackees/reld/releases/tag/linux-linker-competition-v2)
includes the [structured report](https://github.com/zackees/reld/releases/download/linux-linker-competition-v2/report.json),
[raw samples](https://github.com/zackees/reld/releases/download/linux-linker-competition-v2/raw-samples.jsonl),
provenance, locks, and the sealed rendering manifest.

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
- **Observable on demand.** Set `RELD_LOG_ENGINE=1` to log each routing decision, so diagnostics
  and benchmark harnesses can tell which real linker ran and why without polluting normal linker
  stderr.

**What's shipped today vs. designed:**

| | Status |
|---|---|
| Bundling multiple real linkers per platform | **Shipped** — native ELF engine + lld bridge, see "Bridge status" above |
| Routing by platform/format (dispatch to native vs. bridge) | **Shipped** |
| Routing by requested *flags/config* | **Shipped for ELF** — LTO/plugin, ICF, discard-all, warning/color policy, version-script policy, and Cortex-A53 erratum 843419 route to `lld` |
| Capability table per bundled engine | **Shipped, initial set** — extended as more native gaps are identified |
| Fallback ordering when the fast engine lacks a capability | **Shipped** — native ELF first, capable ELF `lld` fallback |
| `--engine=` / `RELD_ENGINE` override | **Shipped** — `reld` or `lld` on ELF; format-specific lld drivers elsewhere |
| Per-decision routing log line | **Shipped** — opt in with `RELD_LOG_ENGINE=1` |

An ELF LTO link is automatically routed to `lld` when reld sees `-flto` or a linker plugin
request, including plugin options found in response files. See [DESIGN.md](DESIGN.md) for the full
routing design and [`agents/docs/polylinker.md`](agents/docs/polylinker.md) for the contributor-
facing summary.

## The four goals

1. **Runs and targets everywhere** — Windows, Linux, macOS. Not "Linux plus ports." The
   polylinker framing extends this claim toward "and supports every requested link
   configuration," via routing — see above for the capability set shipped today.
2. **Incremental** — a one-object change reflows the image instead of rebuilding it.
   Target: **warm relink in low single-digit milliseconds**, largely independent of binary size.
3. **Goal: faster than `wild` in every category** — cold link, warm full link, warm incremental,
   peak RSS, *and* single-threaded. All five, or the goal is not met. The current final-link
   evidence above shows a wall-time win but an RSS tie, so this remains unfulfilled. See
   [DESIGN.md §2.3](DESIGN.md) for why single-threaded is on that list.
4. **Benchmarks are auto-generated and published by CI** — same harness pattern as
   [`zccache`](https://github.com/zackees/zccache), on pinned competitor versions, with
   regressions published rather than hidden.

## Why this is worth doing

Dropping release linking as a goal is not a limitation to apologize for — it is the design
freedom that makes the rest tractable. LTO is deferred, with no PGO/BOLT interaction or
cross-machine and cross-toolchain reproducible-build guarantee. Behavior-preserving changes still
require artifact equivalence: performance-only and output-path changes are byte-identical by
default. See [DESIGN.md §3.1](DESIGN.md) for the distinction and required evidence.

And the evidence says throughput is the wrong thing to chase. On a 46 MB debug Rust binary on
`x86_64-pc-windows-gnu`, `ld.bfd` and `ld.lld` measure **statistically identical**. If a
target's entire link budget is ~1 second, an infinitely fast batch linker saves under a second.
**Incremental is the product; throughput is table stakes.**

Notably, incremental linking sits **above** both Mach-O and Windows on `wild`'s own roadmap —
the clearest signal available that this is the unserved need.

## Non-goals

- Release / shipping builds
- Cross-machine and cross-toolchain reproducible builds as a release guarantee
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
  bash -lc 'RELD_TEST_CONFIG=/src/test-config-ci.toml rustup run --install 1.95.0 cargo test --workspace'
```

See [UPSTREAM.md](UPSTREAM.md) for the tracked source delta.

## License

The code derived from wild is dual-licensed MIT OR Apache-2.0; see [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE), and [NOTICE](NOTICE). The repository's original reld components
remain BSD 3-Clause under [LICENSE](LICENSE).

## Related

- [`soldr`](https://github.com/zackees/soldr) — build tool with content-addressed caching.
  `soldr` makes the compile fast; `reld` is the other half of the loop.
