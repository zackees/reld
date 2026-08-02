# Phase 2 — Prove Linux

No new linker features. This phase converts "we forked something that reportedly works" into
"CI demonstrates it works, under our name, with our oracle." Everything here is measurement.

---

## P2-T1 — Full inherited suite green

wild's 213 ELF test directories, expanded across architectures, running under the Phase 1
harness with reld as the linker under test and `ld.bfd` as the reference.

Scope to `x86_64` and `aarch64` for v0. wild supports five ELF architectures; the other three
(riscv64, loongarch64, ppc64) must remain **compiling and untested** rather than silently
deleted — record them as unverified in the README rather than claiming support.

`Acceptance:` `cargo test --test acceptance -- elf/x86_64 elf/aarch64` green.

## P2-T2 — Self-host

Link reld with reld. The strongest single correctness signal available for one CI job.

```
cargo build --release
cargo build --release --target x86_64-unknown-linux-gnu \
  --config 'target.x86_64-unknown-linux-gnu.rustflags=["-Clink-arg=-fuse-ld=/path/to/ld.reld"]'
```

Then run the reld-built reld and have it link the test suite — a two-generation bootstrap.

`Acceptance:` `bash ci/selfhost.sh` exits 0.

## P2-T3 — Real-world corpus, published as a pass rate

A pinned list of C/C++/Rust projects linked by reld in CI. Publish the pass rate to the
`benchmark-stats` branch alongside the existing benchmark artifacts, under the same honesty
policy as `DESIGN.md` §6: **a regressing number is still published.**

Start with ~15 projects spanning the stressors that matter: heavy C++ templates and COMDAT
(LLVM subset), TLS, symbol versioning (glibc-linked binaries), static+PIE variants, and Rust
crates with build scripts.

A link-success table is more persuasive at this stage than any speed chart, and it can start
publishing long before the speed column has content.

`Acceptance:` `ci/corpus.py --publish` writes `corpus.json` with per-project pass/fail.

## P2-T4 — Populate the benchmark column

`crates/reld-testkit/src/bin/reld-bench.rs` and `.github/workflows/benchmark-stats.yml` already
exist and already publish; the `reld` column currently reads `n/a`.

Add reld to `default_linkers()` and let the nightly job populate it. Report all five categories
from `DESIGN.md` §2.3 — cold link, warm full link, warm incremental, peak RSS, and
single-threaded — with the incremental column reading `n/a` until Phase I2.

Do not suppress unfavourable numbers. Two specific expectations to set now, so they are not
mistaken for regressions later:

- **Single-threaded is a genuine risk.** mold is slower than lld single-threaded; wild's
  architecture is heavily parallel. Expect to lose this category initially.
- **macOS throughput is a losing fight.** Apple's `ld_prime` is 1.4–2× faster than `ld64.lld`.
  The macOS pitch is incremental linking and an open implementation, not raw speed.

`Acceptance:` the nightly job publishes a chart with a populated `reld` column for Linux.

## P2-T5 — Error-message quality baseline

Cheap, high-visibility, and it makes every later phase easier to debug. The daily-driver
difference between linkers is diagnostics, not throughput.

Undefined-symbol errors must name the referencing object *and* demangle the symbol (Rust and
Itanium C++). mold asserts on exactly this in `test/missing-error.sh`, which greps for the
`>>> a.o` "referenced by" line — copy the test, not just the feature.

`Acceptance:` a fixture referencing an undefined `foo()` from `a.o` produces output matching
both `undefined symbol: foo` and `>>> .*a\.o`.
