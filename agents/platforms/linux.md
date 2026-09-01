# Linux / ELF agent guide

This guide owns Linux-specific contributor policy. Read [`../../DESIGN.md`](../../DESIGN.md) §3.1
and the root [`AGENTS.md`](../../AGENTS.md) first.

## Current architecture and references

- Rust 1.95 is the MSRV; normal local and CI builds use the exact 1.95.0 pin, not floating
  `stable`.
- Shared CI runs `python3 ci/check_dependencies.py` before platform builds; an approved dependency
  change must update its baseline in the same reviewed change.
- Fast non-LTO links use `reld`'s native ELF engine inherited from `wild`.
- LTO and other unsupported native capabilities route to the bundled `ld.lld` bridge.
- For inherited native behavior, the artifact reference is the exact pinned `wild` fork commit from
  `DESIGN.md`, or a pinned pre-change `reld` revision when validating a later optimization.
- `bfd`, `lld`, and `mold` are useful compatibility or performance comparators, but they are not
  byte-identity references because each linker has its own ELF layout policy.
- The default Linux `reld` executable uses Rust's system allocator. The exact crates.io
  `mimalloc-pprof` release pinned in the workspace manifest and lockfile is optional and
  compiled only by explicit diagnostic features. Use `--features mimalloc-pprof-profile`
  for sampled profiling or `--features mimalloc-pprof-dhat` for an exact diagnostic run.
  Both modes select the same mimalloc global allocator; internal DHAT never installs a
  second allocator. The default was restored after the #93 evidence found no demonstrated
  wall-time improvement and an inconclusive CPU non-regression interval.
  See [`../../docs/MIMALLOC-PPROF.md`](../../docs/MIMALLOC-PPROF.md) for
  provenance and updates.

## Required evidence

For performance-only native ELF changes, replay the same captured link with identical effective
arguments and environment, then require raw file identity between:

1. serial and parallel implementations when both exist;
2. mmap, buffered, and direct-write output modes when applicable; and
3. the candidate and its pinned `reld`/`wild` baseline.

Run each side twice to verify self-determinism. Compare raw bytes or cryptographic hashes first. If
they differ, preserve the artifacts and inspect the first differing offsets; do not accept execution
success as sufficient.

When an intentional ELF policy change makes identity inappropriate, record the expected changed
fields and compare at least ELF headers, program headers, section headers, section contents,
symbols, dynamic entries, relocations, notes/build ID, and loadable-segment permissions. Use
`readelf`, `llvm-readobj`, or an equivalent committed parser with stable output. Add a focused test
for the policy delta.

Every produced executable must also run natively with exact exit status, stdout, and stderr. Keep
the randomized `reld-difftest` execution comparison as a supplemental oracle; it does not replace
artifact comparison.

### Immutable Clang final-link corpus

The long-form Linux workload is a captured LLVM/Clang final link, separate from the published
five-linker × three-LTO matrix. The checked `ci/clang-link-corpus.lock.json` is the contract for a
GitHub Release `.tar.gz` or `.tar.zst` asset. It records the LLVM source tag, annotated tag object,
and peeled commit; digest-pinned builder image; exact package and toolchain versions; configure and
build argv; archive URL, byte size, and SHA-256; captured linker argv, working directory,
environment, response files, and expected linker output; exact native oracle; and every regular
file in the input closure with its relative path, byte size, and SHA-256. The archive must contain
no symlinks or unrecorded files. Arguments use `{CORPUS}` for the verified extraction root and one
`{OUTPUT}` token for the fixed output path.

Validate a checked lock without downloading the asset:

```sh
uv run --no-project --python 3.12.11 \
  python -m ci.clang_link_replay validate-lock \
  --lock ci/clang-link-corpus.lock.json
```

For pre-publication validation, replay the locally packed archive explicitly; never substitute an
unverified or floating URL, digest, toolchain, cache, or branch:

```sh
uv run --no-project --python 3.12.11 \
  python -m ci.clang_link_replay replay \
  --lock ci/clang-link-corpus.lock.json \
  --archive /absolute/path/to/clang-link-corpus.tar.zst \
  --baseline /absolute/path/to/baseline-reld \
  --candidate /absolute/path/to/candidate-reld \
  --workdir target/clang-link-replay \
  --report clang-replay-evidence/replay-report.json
```

Omit `--archive` only after the lock names the immutable GitHub Release asset. The replay verifies
the archive and complete extracted closure before any link. Its locked direct-link arguments must
include `--no-fork`; compilation, download, extraction, output removal, native execution, and
evidence writing are outside timing. It first links twice with the pinned baseline and twice with
the candidate, runs the exact native oracle after every output, and requires raw identity within
and between both sides. A mismatch retains all four files, both hashes, and the first differing
offset. One excluded candidate warmup supplies the calibration duration; the resulting fixed count
of identical candidate links targets 30 seconds total. The native oracle still runs after every
timed link, after its timer stops.

The manual-only `.github/workflows/clang-link-replay.yml` owns hosted replay. It builds baseline
`6ec92be6674d026e74f7524271fbcbce68b50a39` and the candidate from separate non-git source archives
with Rust 1.95.0, requires identical `non-git-build` linker-version output so `.comment` provenance
cannot create a false artifact delta, validates the checked lock, downloads its asset, runs the
gate, and uploads the report plus retained identity-failure evidence. Do not add this workload to
the public benchmark matrix.

### Linux linker competition evidence

`.github/workflows/linux-linker-competition.yml` is a separate Linux-only, same-run competitive
evidence workflow. It is deliberately a direct `ubuntu-24.04` VM job rather than a job container:
the replay must receive an explicitly preflighted, delegated cgroup-v2 root so the measurement
runner can validate process-tree RSS. Because an Actions step begins outside that subtree, the
workflow invokes its cgroup collector self-test and replay through the pinned absolute venv Python
under non-interactive `sudo`; this root-scoped boundary is required for a process launcher to move
itself and its descendants into measurement children. The root-scoped replay preserves only the
explicit hosted-provenance allowlist (`GITHUB_ACTIONS`, `GITHUB_SHA`, `GITHUB_RUN_ID`,
`GITHUB_RUN_ATTEMPT`, `RUNNER_OS`, `RUNNER_ARCH`, `RUNNER_ENVIRONMENT`, `ImageOS`, `ImageVersion`,
and `BASELINE_SHA`); it must never use blanket sudo environment preservation. Pull requests run only when their *head* branch starts with
`perf/linux/`; a manual dispatch requires a full immutable 40-hex `baseline_sha`. A PR baseline is
always the exact `github.event.pull_request.base.sha`, never a branch name or floating revision.

The workflow builds both reld binaries from separate non-git source archives under the same pinned
Rust 1.95.0 toolchain, provisions external linkers from the checked
`ci/linux-linker-comparators.lock.json`, then invokes:

```sh
python -m ci.linux_linker_competition validate-lock \
  --lock ci/linux-linker-comparators.lock.json
python -m ci.linux_linker_competition provision \
  --lock ci/linux-linker-comparators.lock.json --output-dir <comparators-dir>
python -m ci.linux_linker_competition replay \
  --corpus-lock ci/clang-link-corpus.lock.json \
  --comparator-lock ci/linux-linker-comparators.lock.json \
  --baseline <baseline-reld> --candidate <candidate-reld> \
  --comparators-dir <comparators-dir> --cgroup-root <delegated-cgroup-root> \
  --workdir <workdir> --report <report.json> --samples 12 --warmups 2
```

The competition runner first enforces the four-run raw reld baseline/candidate identity gate and
the two-run self-determinism/native oracle gate for every external linker. It records default
product behavior, with wall time and peak *process-tree RSS* as independent co-primary metrics.
`memory.peak` and CPU data are whole-tree diagnostics, never relabelled as RSS. Do not substitute
parent-only `/usr/bin/time` or `getrusage` data: forked linkers make it materially wrong.
Competitive replay uses exactly 12 measured rounds: two complete six-contender Williams blocks.
The harness accepts only a sample count of at least 12 and divisible by six, preserving balanced
pairwise ordering, contender position, and predecessor/successor carryover across the blocks.
Wild is an external performance comparator only: the exact pre-change reld baseline is the
artifact reference, and no Wild/reld artifact-equivalence claim is made because reld intentionally
changed its build-ID/output-layout policy in PR #76.

The renderer produces one grouped dual-axis wall-time/RSS graph. Its fixed contender order is
`bfd`, `lld`, `mold`, `wild`, `reld baseline`, then `reld candidate`. Each contender group has a
solid wall-time bar first, read against the zero-based left axis in seconds, followed by a
diagonally hatched peak process-tree RSS bar, read against the independent zero-based right axis
in MiB. The matching contender hue is retained for both bars; solid versus hatch is the required
non-colour metric distinction. Exact medians and bootstrap 95% confidence intervals are annotated
on both bars. The accessible HTML figure description and full exact-value table are required
nonvisual sources for the same values, units, bar order, and axis ownership.

This grouped presentation is not normalization or scalarization: it adds no combined, weighted,
or composite score, and performance claims still require the independent confidence rules for both
metrics. The workflow uploads the schema-versioned report, raw JSONL, provenance, accessible HTML,
PNG, generated job summary, and (only on failure) retained mismatch artifacts. Inspect the hosted
PNG before publication for bar order, left/right zero-based axis labels, solid/hatched legend,
exact median/CI annotations, clipping, and non-overlap. A successful combined-layout run is
published as an immutable `linux-linker-competition-v2` evidence bundle (PNG, HTML, report, raw
samples, provenance, locks, summary, and completion manifest); v1 remains available as its
historical two-panel evidence. The workflow has no schedule and never writes benchmark history;
hosted results support only same-run comparisons.

Prefer the complete `RelWithDebInfo` closure. The standard public `ubuntu-24.04` runner has a
measured 16 GB RAM and 14 GB SSD, and each GitHub Release asset must remain below 2 GiB; the checked
corpus must fit all three constraints with operational headroom. These ceilings are not permission
for an undocumented transform, and this guide does not invent a smaller fixed gate that the harness
does not enforce. If the complete closure cannot fit, the only accepted reduced variant is produced
by deterministic GNU binutils 2.40 `strip --strip-debug` applied to copied ELF `.o` and `.a` inputs
only. Retain complete pre-strip and post-strip SHA-256 manifests, and record the exact strip
package, version, and argv in the lock. Keep every other link argument fixed and apply the same
raw-artifact and native gates after the transform. Such a corpus represents a non-debug Clang final
link and must not support claims about debug-heavy link performance.

For allocator changes, build and replay the same captured ELF link in the default,
sampled-profiling, and exact-DHAT configurations. First establish two-run self-determinism for each
configuration, then compare the default artifact with the pinned pre-change system-allocator
baseline as raw bytes; if a difference appears, preserve it and apply the §3.1 structural and
native-execution requirements before accepting it. Profiling is diagnostic only and must not be
used for a performance claim without the same evidence. Authoritative timing requires the default
hook-free build plus proof that sampled pprof and exact DHAT are runtime-off. Profile-collection
runs are diagnostic only and must never be timing evidence.
Set `RELD_SYSTEM_BASELINE` to the pinned pre-change executable and run
`bash ci/allocator_equivalence.sh` inside the Bosn Linux stack for the focused two-run raw-byte
identity and native-execution proof across the pinned system baseline, default, sampled-pprof, and
exact-DHAT builds. The script clears all supported profiler activation/dump environment controls
before invoking every linker.

The authoritative allocator decision uses `bash ci/allocator_benchmark.sh` at revision `e0868677`.
It verifies and archives the exact allocator-change Git tree at `e0868677` and the exact pinned
pre-change system-allocator tree at `e2d6be5a` (no worktree). The intervening linker-relevant diff
is allocator selection in the manifests and `main`; linker implementation source is unchanged.
It builds both release binaries with the pinned lockfiles/toolchain, rejects profiler
environment contamination and sampled-pprof symbols, and then benchmarks the three frozen replay
profiles. Each mode first links twice for raw identity and exact native-output validation. Timing
uses two excluded warmups and at least ten rotating/interleaved samples per cell, recording wall
and CPU time, peak RSS, dispersion, bootstrap confidence intervals, raw samples, order, binary and
artifact hashes, plus machine/toolchain provenance. The resulting JSON is evidence for #93; pprof
and DHAT collection must be performed separately and must never be added to its timing cells.

## Consumer acceptance

The three-platform consumer job builds the host-named `reld` ELF driver through `setup-soldr`, then
links the pinned Rust and C/C++ consumers through that release executable. Set
`RELD_INVOCATION_LOG` for every build
and require a successful JSONL record naming each expected final artifact and the native ELF route.
This invocation record is mandatory even when verbose compiler output appears to name reld. Then
run `xsv count`, the full pinned PCRE2 CTest suite, and the C++ name-mangling CTest natively.

## Where Linux policy lives

- Native implementation: `crates/reld-core/src/`
- Differential runner: `crates/reld-testkit/src/bin/reld-difftest.rs`
- Benchmark replay and execution oracle: `ci/benchmark_runner.py`
- Immutable Clang corpus lock/replay: `ci/clang_link_replay.py` and
  `.github/workflows/clang-link-replay.yml`
- Competitive Linux replay and grouped dual-axis renderer: `ci/linux_linker_competition.py`,
  `ci/linux_linker_competition_render.py`, and
  `.github/workflows/linux-linker-competition.yml`
- Consumer acceptance driver: `ci/consumer_acceptance.py`
- Linux acceptance and differential CI: `.github/workflows/ci.yml`
- Three-platform consumer acceptance: `.github/workflows/linker-artifacts.yml`
- Published benchmark matrix: `.github/workflows/benchmark-stats.yml`

Do not publish a speed result from a performance-only change when artifact identity has not been
checked. If the current harness lacks the required gate, add the gate or clearly mark the result as
diagnostic rather than accepted.
