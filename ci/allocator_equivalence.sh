#!/usr/bin/env bash
# Focused allocator-mode artifact equivalence and native execution proof.
set -euo pipefail

baseline="${RELD_SYSTEM_BASELINE:-/bosn/target/phase1a-system/debug/reld}"
if [[ ! -x "${baseline}" ]]; then
  echo "set RELD_SYSTEM_BASELINE to the pinned pre-change reld executable" >&2
  exit 2
fi

rm -rf /tmp/reld-allocator-equivalence
mkdir -p /tmp/reld-allocator-equivalence
trap 'rm -rf /tmp/reld-allocator-equivalence' EXIT

printf '.global _start\n.text\n_start:\n  mov $60, %%rax\n  xor %%rdi, %%rdi\n  syscall\n' \
  >/tmp/reld-allocator-equivalence/main.s
clang -c /tmp/reld-allocator-equivalence/main.s -o /tmp/reld-allocator-equivalence/main.o

cargo build --locked -p reld --bin reld
cp /bosn/target/debug/reld /tmp/reld-allocator-equivalence/reld-default
cargo build --locked -p reld --bin reld --features mimalloc-pprof-profile
cp /bosn/target/debug/reld /tmp/reld-allocator-equivalence/reld-pprof
cargo build --locked -p reld --bin reld --features mimalloc-pprof-dhat
cp /bosn/target/debug/reld /tmp/reld-allocator-equivalence/reld-dhat
cp "${baseline}" /tmp/reld-allocator-equivalence/reld-system

for mode in system default pprof dhat; do
  for run in 1 2; do
    env -u MIMALLOC_PROF -u MIMALLOC_PROF_DUMP_AT_EXIT -u MIMALLOC_DHAT \
      -u MIMALLOC_DHAT_DUMP_AT_EXIT \
      RELD_DHAT_OUTPUT="/tmp/reld-allocator-equivalence/${mode}-${run}.dhat.json" \
      "/tmp/reld-allocator-equivalence/reld-${mode}" -static \
      -o "/tmp/reld-allocator-equivalence/${mode}-${run}" \
      /tmp/reld-allocator-equivalence/main.o
    "/tmp/reld-allocator-equivalence/${mode}-${run}"
  done
  cmp "/tmp/reld-allocator-equivalence/${mode}-1" "/tmp/reld-allocator-equivalence/${mode}-2"
done

cmp /tmp/reld-allocator-equivalence/system-1 /tmp/reld-allocator-equivalence/default-1
cmp /tmp/reld-allocator-equivalence/default-1 /tmp/reld-allocator-equivalence/pprof-1
cmp /tmp/reld-allocator-equivalence/default-1 /tmp/reld-allocator-equivalence/dhat-1
sha256sum /tmp/reld-allocator-equivalence/{system,default,pprof,dhat}-{1,2}

if env -u MIMALLOC_PROF -u MIMALLOC_PROF_DUMP_AT_EXIT -u MIMALLOC_DHAT \
  -u MIMALLOC_DHAT_DUMP_AT_EXIT \
  RELD_DHAT_OUTPUT=/tmp/reld-allocator-equivalence/failed-link.dhat.json \
  /tmp/reld-allocator-equivalence/reld-dhat \
  -static -o /tmp/reld-allocator-equivalence/failed-link \
  /tmp/reld-allocator-equivalence/does-not-exist.o; then
  echo "expected the missing-input link to fail" >&2
  exit 1
fi
test -s /tmp/reld-allocator-equivalence/failed-link.dhat.json
