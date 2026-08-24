# Link benchmark workload

This workspace contains one cross-platform artifact-auditing CLI used only as a final-link
benchmark. It deliberately exercises a representative set of Rust application facilities:
structured parsing, SQLite, graph analysis, parallel iteration, hashing, templating, image
encoding, archive/compression codecs, and construction of a TLS-capable HTTP client.
It also retains a compiled policy-signature table, mirroring scanners that ship their rules in the
executable, so every platform performs enough native section work for the significance gate.

The benchmark compiles this project once for each Cargo LTO profile, captures rustc's real final
linker command, and replays only that command. Compilation and Rust LTO preparation are never
included in measured final-link latency.
