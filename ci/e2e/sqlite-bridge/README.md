# sqlite-bridge e2e fixture

A standalone two-crate workspace (`crates/dbcore` + `crates/app`) that links `rusqlite`
(`bundled`), `serde`, `serde_json`, and `anyhow`. It opens an in-memory SQLite database,
seeds rows, round-trips data through serde, and prints `OK` on success.

CI (`.github/workflows/ci.yml`, `windows-msvc` leg) builds and runs this fixture with
`reld-link.exe` as the MSVC linker to prove the Windows COFF bridge end-to-end. This
workspace is excluded from the repo root workspace (`Cargo.toml` `[workspace].exclude`)
so it never affects root builds, clippy, or `cargo deny`.
