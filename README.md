# reld

*relink. reweld. reload.*

A cross-platform incremental linker built for the **inner dev loop** — not for release builds.

> Status: **design / exploration.** No working linker yet. This repo currently holds the thesis,
> the research that motivated it, and the target design. See [Prior art](#prior-art) for why this
> is being started rather than contributed to an existing project.

## Thesis

Every fast linker in existence — `lld`, `mold`, `wild`, `radlink` — optimizes the **final link**.
They are batch linkers that happen to be quick. Correctness at release quality is the bar they
must clear, and clearing it is what consumes years of engineering and what has killed prior
efforts outright.

`reld` inverts the priority:

- **The edit → link → run loop is the product.** Nothing else is.
- **Release linking is an explicit non-goal.** Ship with your platform's linker.
- **Incremental is the architecture, not a feature.** The image is *reflowed*, not rebuilt.

Dropping release linking as a goal is not a limitation to apologize for. It is the design
freedom that makes the rest tractable: no LTO, no PGO/BOLT interaction, no
determinism-for-reproducible-builds constraint, and a correctness bar set at "runs correctly on
this developer's machine right now" instead of "byte-identical and correct for every downstream
consumer forever."

## Non-goals

Stated up front, because they define the project more than the goals do:

- Release / shipping builds
- LTO
- Byte-for-byte reproducible output
- Being a drop-in replacement for `ld` in a distro build system
- Beating any linker on a cold, from-scratch link

If you need any of the above, use `lld` or your platform linker. `reld` is for the 200th
rebuild of the day, not the one that goes to customers.

## Goals

- **Warm relink in the low milliseconds** for a single-object change
- **Cross-platform**: ELF, PE/COFF, Mach-O — in that order of likelihood, not commitment
- **Works everywhere**, including Windows hosts as first-class, not an afterthought
- Persistent/daemon mode so the symbol graph stays hot between edits

## Prior art

`reld` exists because of what the landscape actually looks like, not in ignorance of it:

- **[mold](https://github.com/rui314/mold)** — ELF-only by construction. The author explicitly
  declined PE/COFF and recommends `lld` instead. Its one non-ELF backend (Mach-O) was removed
  and commercialized as `sold`, which is now archived.
- **[wild](https://github.com/wild-linker/wild)** — the best-architected base available: a real
  `Platform` trait with three backends and a genuinely format-neutral layout core. Mach-O and
  Wasm ports are in flight. Notably, **incremental linking sits above both Mach-O and Windows on
  its own roadmap** — which is the strongest signal that this is the unserved need.
- **[radlink](https://github.com/EpicGamesExt/raddebugger)** — a real, shipping, parallel PE/COFF
  linker (MIT). Excellent reference for parallel COFF parsing and lock-free symbol tables.
  MSVC-flavored and PDB-centric; no format abstraction.
- **[lld](https://lld.llvm.org/)** — four largely separate linkers sharing ~2.5% of their code.
  It began as a unified multi-format design and deleted that abstraction layer in 2021
  (244 files, 36,441 lines). Worth knowing before attempting unification again.

**None of them do incremental.** That is the gap.

## License

BSD 3-Clause. See [LICENSE](LICENSE).

## Related

- [`soldr`](https://github.com/zackees/soldr) — Rust/C++ build tool with content-addressed caching.
  `soldr` makes the compile fast; `reld` is the other half of the loop.
