# Polylinker: routing model for contributors

This is a short orientation note for anyone (agent or human) adding a new linker backend or
touching dispatch code. Full design: [`DESIGN.md`](../../DESIGN.md) §4.4–§4.5. Locked decision:
[`docs/plan/01-DECISIONS.md`](../../docs/plan/01-DECISIONS.md) D13. Source issue:
[#30](https://github.com/zackees/reld/issues/30) (extends B8 in
[#17](https://github.com/zackees/reld/issues/17) and the daemon router in
[#19](https://github.com/zackees/reld/issues/19)).

## The idea

`reld` is not one linker; it's a **polylinker** — a single binary that bundles multiple real
linker engines per platform and routes each link request to whichever bundled engine can satisfy
it. Two engines exist today: reld's own native engine (Linux/ELF) and the `lld` bridge
(Windows/COFF via `lld-link`, macOS/Mach-O via `ld64.lld`). The framing generalizes: as more
engines get bundled (e.g. `radlink` per `DESIGN.md` §7), routing decides per-link which one runs,
rather than reld growing a fork of itself per engine.

The point of the framing: a capability reld's fast default engine doesn't have (the leading
example is LTO) doesn't have to mean "reject the flag." It can mean "hand this link to a bundled
engine that already has it." reld's own engine still owns zero LTO codegen; the user still gets
an LTO-capable link.

## Shipped vs. designed — read this before assuming either

| Layer | Status |
|---|---|
| Bundling more than one real linker engine per platform | **Shipped.** Native engine (Linux) + lld bridge (Windows, macOS). |
| Routing by platform/format, decided once at dispatch | **Shipped.** This is today's entire routing logic — it is not flag-aware. |
| Capability table (per-engine declared support for LTO, GC-sections, ICF, PDB/DWARF, incremental, formats, ...) | **Design only.** No such table exists in code yet. |
| Flag-aware router (inspect argv/env, classify requested config, pick engine) | **Design only.** Not implemented. Tracked as B8 (#17) / #30 / #19. |
| Fallback ordering when the default engine lacks a capability | **Design only.** |
| `--engine=` / `RELD_ENGINE` explicit override | **Design only.** |
| Per-decision routing log line (`reld: engine=<name> (reason: ...)`) | **Design only.** The bridge does log a `reld: delegating to <linker> (bridge mode)` note today (§4.4) — that's the shipped precedent the design generalizes, not the routing log itself. |
| LTO delegated to a capable engine on request | **Design only.** Today, LTO flags are rejected/ignored with a diagnostic (D13); they are not routed anywhere yet. |

If you're implementing B8/#19: this table is your scope. If you're just reading the docs to
understand what reld does *today*, only the first two rows are real.

## The rule for new backends

When a new bundled engine is added (a new bridge target, or eventually a new native format
backend), it goes in **behind the capability table** — declare what it supports, let the router
(once it exists) pick it when appropriate — rather than as a parallel, hand-forked dispatch path
or a duplicated set of `bail!`/flag-handling arms copied from an existing engine's integration.
The two `bail!` sites replaced by the lld bridge (`DESIGN.md` §4.4, historically
`crates/reld-core/src/lib.rs:266-267`) are the precedent for "one dispatch point per format, not
one per engine."

Concretely, avoid:

- Forking a second copy of argv-parsing / flag-classification logic per engine.
- Hardcoding a new engine's selection logic ad hoc at a call site instead of extending the
  (eventual) capability table.
- Silent behavior differences between engines for the same requested configuration — if two
  bundled engines disagree on what a flag means, that's a capability-table entry and a routing
  decision, not something to paper over.

## Non-negotiables carried over from the design

- **Never a silent mislink.** An unroutable/unsupported configuration is a loud, specific error,
  not a best-effort fallback that silently drops a correctness-affecting flag.
- **LTO and the incremental daemon path are mutually exclusive** on a given link (same as gold
  and MSVC) — the router must not try to combine them.
- **Routing decisions are observable.** Whatever engine actually ran, and why, should be visible
  to the user (and, for the benchmark harness, recorded in the published data — see `DESIGN.md`
  §6's `mode`/`engine` field).
