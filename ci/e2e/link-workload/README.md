# Link benchmark workload

This workspace contains one cross-platform artifact-auditing CLI used only as a final-link
benchmark. It deliberately exercises a representative set of Rust application facilities:
structured parsing, SQLite, graph analysis, parallel iteration, hashing, templating, image
encoding, archive/compression codecs, and construction of a TLS-capable HTTP client.
It also retains a target-calibrated compiled policy-signature table, mirroring scanners that ship
platform-specific rules in the executable, so every platform performs enough native section work
for the significance gate without exceeding its hosted compiler's resource envelope.

The benchmark compiles this project once for each Cargo LTO profile, captures rustc's real final
linker command, and replays only that command for every measured linker. It finishes every replay
for one configuration and releases those retained inputs before compiling the next configuration,
so the three large LTO captures never accumulate on a hosted runner. Compilation, Rust LTO
preparation, capture cleanup, and executable validation are never included in measured final-link
latency.
