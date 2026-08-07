# Benchmark assets: frozen linker-object corpora

Coordination layer for the "compile once, link every iteration" benchmark (meta issue #57,
corpus #55, LTO sections #56). Linker corpora are built **once per platform per configuration**,
frozen, and published so the benchmark only ever times the **link** — never compilation.

## Flow

```
build once  ->  pack (zstd)  ->  manifest.json  ->  publish (benchmark-assets branch)
                                       |
consumer:                             v
   resolve -> fetch (verify sha256) -> extract -> reld-bench --replay-corpus   (zero compile)
```

- `ci/benchmark_assets.py` — pack/extract archives, build/validate/resolve `manifest.json`,
  and fetch+verify+extract a corpus. Pure logic is unit-tested in `ci/tests/test_benchmark_assets.py`.
- `reld-bench --replay-corpus <dir>` — reads `<dir>/corpus.json` and times the link across every
  linker (and reld's shim) against the frozen objects. No compilation happens in the loop.
- `.github/workflows/benchmark-assets.yml` — `validate` proves the pipeline end-to-end on PRs;
  `build` + `publish` regenerate corpora and push the `benchmark-assets` branch on the default
  branch (dispatch/schedule).

## Storage decision (adopted)

**One orphan `benchmark-assets` branch** that doubles as the assets folder / static www site,
with a single canonical `manifest.json` at its root and one archive per platform per
configuration beneath it:

```
benchmark-assets/                 # orphan branch, .nojekyll
  manifest.json                   # canonical index — single source of truth
  x86_64-linux/quick/corpus.tar.zst
  x86_64-pc-windows-msvc/quick/corpus.tar.zst
  aarch64-apple-darwin/quick/corpus.tar.zst
  # ... thin-lto / full-lto configurations added by #56
```

Canonical manifest URL:
`https://raw.githubusercontent.com/zackees/reld/benchmark-assets/manifest.json`

Chosen over per-platform `benchmark-assets-<platform>` branches for the simplest consumer story
and a single www site. Escalate to per-platform branches only if blob size or force-push
contention demands it (see #57).

## `manifest.json` schema (v1)

```json
{
  "schema_version": 1,
  "generated_at": "<ISO8601>",
  "corpus_version": "<e.g. bevy version or demo-v1>",
  "assets": [
    {
      "platform": "x86_64-linux",
      "configuration": "quick",              // quick | thin-lto | full-lto
      "archive_path": "x86_64-linux/quick/corpus.tar.zst",
      "archive_url": "https://raw.githubusercontent.com/zackees/reld/benchmark-assets/x86_64-linux/quick/corpus.tar.zst",
      "sha256": "<64 hex>",
      "bytes": 12345,
      "corpus_json": "corpus.json",
      "toolchain": {}
    }
  ]
}
```

`manifest.json` is the single source of truth for consumers; archives are content-addressed by
sha256 and keyed on `(platform, configuration)`. A checksum mismatch on fetch is a hard failure —
the benchmark never links against a corrupt or wrong corpus.

## `corpus.json` (inside each archive)

```json
{
  "schema_version": 1,
  "platform": "x86_64-linux",
  "configuration": "quick",
  "cc": "clang",
  "objects": ["objs/u0.o", "objs/main.o"],
  "extra_link_args": [],
  "output_name": "app"
}
```

`reld-bench --replay-corpus` links `objects` (relative to the extracted corpus dir) with `cc`,
swapping `-fuse-ld=<linker>` per series. Extra linker inputs (native libs, `-l…`) go in
`extra_link_args`. Unknown fields are ignored so the recipe can carry extra metadata.

## Archive codec

Codec is chosen by extension: `.tar.zst` (production, via the `zstd` CLI — fast, high ratio) and
`.tar.gz` (stdlib, used by the hermetic tests). The manifest records each archive by filename +
sha256 regardless of codec.

## Scope note

This layer is demonstrated end-to-end with a small synthetic corpus. The real Bevy corpus lands
in #55 and the thin/full-LTO configurations in #56 — both drop straight into this manifest +
publish + replay machinery unchanged.
