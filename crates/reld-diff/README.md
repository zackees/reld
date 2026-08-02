# reld-diff

Linker-diff is a command-line utility that diffs two ELF binaries (shared objects or executables).
At least one of the binaries being diffed needs layout information as can optionally be produced by
the Reld linker.

## Usage

The easiest way to use reld-diff is to first make sure it's installed into the same directory as
the reld linker, then build with the environment variable `RELD_REFERENCE_LINKER` set to the name of
another linker. e.g.

```sh
RELD_REFERENCE_LINKER=ld cargo test
```

When this variable is set, each time the reld linker is invoked, it'll call the specified linker
then run reld-diff on the result.
