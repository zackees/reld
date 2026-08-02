# reld

*relink. reweld. reload.*

**A fork of the [`wild`](https://github.com/wild-linker/wild) linker** — with Windows,
Linux, and macOS as co-equal targets, incremental linking as the architecture, and a mandate to
beat `wild` in every measured category.

> **Status: ELF/Linux linking works today, inherited from wild.** Windows/COFF and macOS/Mach-O
> are not implemented. Every performance number below remains a **target**, not a measurement.
> See [DESIGN.md](DESIGN.md).

<!-- BENCHMARK:BEGIN -->
[![Latest reld link benchmark](https://raw.githubusercontent.com/zackees/reld/benchmark-stats/benchmark-link.jpg)](https://github.com/zackees/reld/tree/benchmark-stats)

*Auto-generated nightly by [`benchmark-stats.yml`](.github/workflows/benchmark-stats.yml) and
published to the [`benchmark-stats` branch](https://github.com/zackees/reld/tree/benchmark-stats),
alongside `latest.json` and `history.jsonl`. The `reld` column remains `n/a` until Phase 2's
CI-generated measurement lands; Phase 0 does not publish unmeasured results. With
[`soldr`](https://github.com/zackees/soldr) installed, reproduce locally with
`soldr cargo run --release --bin reld-bench`.*
<!-- BENCHMARK:END -->

## What it is

`reld` is a source fork of `wild` at commit
`5793935f1d8b05b9a978ce2089e16e718072e9a9`. The inherited ELF linker works now; the scope then
expands toward native Windows and macOS backends and incremental linking:

| | `wild` | `reld` |
|---|---|---|
| Platforms | Linux / ELF | Linux / ELF now; **Windows and macOS planned** |
| Incremental linking | on the roadmap | **the architecture** |
| Optimizes for | the final link | the **edit → link → run** loop |
| Release-quality output | required | **explicit non-goal** |

## The four claims

1. **Runs and targets everywhere** — Windows, Linux, macOS. Not "Linux plus ports."
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
would slow down the thing this project exists to deliver. Until then, LTO flags are rejected
with a clear diagnostic rather than silently mislinked.

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
