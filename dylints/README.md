# Dylint lints

## `ban_platform_cfg_outside_boundary`

Keeps host-platform selection and native OS APIs inside reld-core's `platforms` facade.

Everywhere in the workspace (`crates/**`: production code, unit-test modules, integration tests,
bins, and the neutral facade leaves `crates/reld-core/src/platforms/{fs,host,linker_plugin,path,process}.rs`)
the lint denies:

- `#[cfg]`, `#[cfg_attr]`, `#![cfg]`, `#![cfg_attr]`, `cfg!()`, and `cfg_select!` mentioning
  `windows`, `unix`, `target_os`, `target_family`, `target_env`, `target_abi`, `target_vendor`,
  or `target_pointer_width`;
- native OS references: `std::os::{unix,windows,linux,macos,fd,wasi}`, `windows_sys`,
  `windows::Win32`, `libc::`;
- outside `crates/reld-core/src/platforms/`, direct references to the concrete trees
  (`platform_imp`, `platform_unix`, `platform_linux`, `platform_macos`, `platform_illumos`,
  `platform_other_unix`, `platform_win`, `platform_wasi`).

`feature = "..."`, `test`, `debug_assertions`, `target_arch`, and `target_endian` are allowed
everywhere: reld is a cross-target linker, so `target_arch` selects the linker target, not the host.

The only exempt files are:

- `crates/reld-core/src/platforms/mod.rs` (the `cfg_select!` selector);
- `crates/reld-core/src/platforms/platform_<tree>.rs` and
  `crates/reld-core/src/platforms/platform_<tree>/**` for `<tree>` in `unix`, `linux`, `macos`,
  `illumos`, `other_unix`, `win`, `wasi`.

There is no baseline and no other exemption. Other code uses
`reld_core::platforms::{fs,host,linker_plugin,path,process}`. `crate::platform` (no `s`) is the
unrelated linker-format trait.

Sources are scanned pre-expansion, so code cfg'd away on the linting host is still checked and a
single Linux CI host covers every platform. Files loaded only through a feature-gated `mod`/`path`
need that feature state active, so CI lints both the default and `--no-default-features` sets.

### Running locally

Install the lint's nightly (from `ban_platform_cfg_outside_boundary/rust-toolchain.toml`) and the
Dylint tools:

```sh
rustup toolchain install nightly-2026-05-28 --profile minimal \
  --component rustc-dev --component llvm-tools-preview --component rust-src --component rustfmt
cargo install cargo-dylint --version 6.0.3 --locked
cargo install dylint-link --version 6.0.3 --locked
```

Test the lint (unit tests and `ui/` fixtures):

```sh
cd dylints/ban_platform_cfg_outside_boundary
cargo fmt --check
cargo test
```

Lint the workspace from the repository root:

```sh
cargo dylint --path dylints/ban_platform_cfg_outside_boundary --workspace -- --all-targets
cargo dylint --path dylints/ban_platform_cfg_outside_boundary --workspace -- --all-targets --no-default-features
```

After an intentional diagnostic change, update the `ui/*.stderr` expectations to the new
`cargo test` output and review the diff. Fixtures without a `.stderr` file must lint cleanly.
