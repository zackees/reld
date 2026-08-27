#!/usr/bin/env bash
# Build the exact pre-allocator baseline and current candidate, then run the correctness-gated A/B.
set -euo pipefail

candidate_revision=e08686773514edf0b30a88e5950a6071d3caf373
candidate_tree=7162acc8005911d79aa85397a20a6744d27a09b8
candidate_archive_sha256=53ba911ffc1ffe65e806c47b066064cae76ab461d97a156b4a4e86fa37f3211f
baseline_revision=e2d6be5ae31350862c562d24da01e2147cd5c125
baseline_tree=24bb072b12b3e2e49dfe8e729195f51bc5f8e93d
baseline_archive_sha256=e05a472ad5c2dcc4b3e3e2293c1332f255efd491bc256557c454b3c856fda640
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="${root}/target/allocator-benchmark"
output="${RELD_ALLOCATOR_BENCHMARK_OUTPUT:-${state}/results.json}"
source_archive="${RELD_ALLOCATOR_SOURCE_ARCHIVE:-${state}/candidate-e0868677.tar}"
baseline_archive="${RELD_ALLOCATOR_BASELINE_ARCHIVE:-${state}/baseline-e2d6be5a.tar}"

test "$(git -C "${root}" rev-parse "${candidate_revision}^{tree}")" = "${candidate_tree}"
if [[ ! -f "${source_archive}" ]]; then
  mkdir -p "$(dirname "${source_archive}")"
  git -C "${root}" archive --format=tar --output="${source_archive}" "${candidate_revision}"
fi
echo "${candidate_archive_sha256}  ${source_archive}" | sha256sum --check --status
test "$(git -C "${root}" rev-parse "${baseline_revision}^{tree}")" = "${baseline_tree}"
if [[ ! -f "${baseline_archive}" ]]; then
  mkdir -p "$(dirname "${baseline_archive}")"
  git -C "${root}" archive --format=tar --output="${baseline_archive}" "${baseline_revision}"
fi
echo "${baseline_archive_sha256}  ${baseline_archive}" | sha256sum --check --status

for variable in MIMALLOC_PROF MIMALLOC_PROF_ACTIVE MIMALLOC_PROF_DUMP_AT_EXIT MIMALLOC_PROF_PREFIX MIMALLOC_PROF_SAMPLE_INTERVAL MIMALLOC_PROF_SAMPLE_RATE MIMALLOC_PROF_ACCUM MIMALLOC_PROF_BT_MAX MIMALLOC_PROF_MAX_BYTES MIMALLOC_PROF_SEED MIMALLOC_PROF_DUMP_FORMAT MIMALLOC_DHAT MIMALLOC_DHAT_DUMP_AT_EXIT MIMALLOC_DHAT_MAX_BYTES RELD_DHAT_OUTPUT; do
  if [[ -v "${variable}" ]]; then
    echo "profiler environment contamination: ${variable}" >&2
    exit 2
  fi
done

mkdir -p "${state}"
baseline_source=
candidate_source=
trap 'python3 "${root}/ci/allocator_source_cleanup.py" "${baseline_source}" "${candidate_source}"' EXIT
baseline_source="$(mktemp -d "${state}/baseline-source-e2d6be5a.XXXXXXXX")"
candidate_source="$(mktemp -d "${state}/candidate-source-e0868677.XXXXXXXX")"
tar -xf "${baseline_archive}" -C "${baseline_source}"
tar -xf "${source_archive}" -C "${candidate_source}"
printf '%s\n' "${baseline_tree}" >"${baseline_source}/.reld-source-tree"
printf '%s\n' "${candidate_tree}" >"${candidate_source}/.reld-source-tree"

for variable in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CC CFLAGS CXX CXXFLAGS CARGO_BUILD_RUSTFLAGS CARGO_TARGET_DIR; do
  if [[ -v "${variable}" ]]; then
    echo "build environment contamination: ${variable}" >&2
    exit 2
  fi
done

common_env=(
  env -i
  "PATH=${RELD_PINNED_CARGO_BIN:-/usr/local/cargo/bin}:/usr/bin:/bin"
  "HOME=/tmp/reld-allocator-home"
  "CARGO_HOME=${state}/cargo-home"
  "RUSTUP_HOME=${RELD_PINNED_RUSTUP_HOME:-/usr/local/rustup}"
  "RUSTUP_TOOLCHAIN=1.95.0"
  "CC=/usr/bin/clang"
  "CXX=/usr/bin/clang++"
  "SOURCE_DATE_EPOCH=0"
)
mkdir -p /tmp/reld-allocator-home
mkdir -p "${state}/cargo-home"
test "$(command -v cargo)" = "${RELD_PINNED_CARGO_BIN:-/usr/local/cargo/bin}/cargo"
test -x "${RELD_PINNED_RUSTUP_HOME:-/usr/local/rustup}/toolchains/1.95.0-x86_64-unknown-linux-gnu/bin/rustc"

"${common_env[@]}" CARGO_TARGET_DIR="${state}/baseline-target" cargo build --locked --release --no-default-features --features fork,plugins,zstd --manifest-path "${baseline_source}/Cargo.toml" -p reld --bin reld
"${common_env[@]}" CARGO_TARGET_DIR="${state}/candidate-target" cargo build --locked --release --no-default-features --features fork,plugins,zstd --manifest-path "${candidate_source}/Cargo.toml" -p reld --bin reld
"${common_env[@]}" cargo tree --locked --manifest-path "${candidate_source}/Cargo.toml" -p reld -e features >"${state}/candidate-feature-tree.txt"
if grep -Eq 'mimalloc-pprof feature "pprof"|mimalloc-pprof/(pprof|dhat)' "${state}/candidate-feature-tree.txt"; then
  echo "profiling feature present in authoritative candidate" >&2
  exit 2
fi

"${common_env[@]}" python3 -m ci.allocator_benchmark \
  --baseline "${state}/baseline-target/release/reld" \
  --candidate "${state}/candidate-target/release/reld" \
  --feature-proof "${state}/candidate-feature-tree.txt" \
  --source-revision "${candidate_revision}" \
  --source-tree "${candidate_tree}" \
  --source-archive "${source_archive}" \
  --baseline-archive "${baseline_archive}" \
  --manifest "${candidate_source}/ci/e2e/link-workload/Cargo.toml" \
  --output "${output}" \
  "$@"

echo "allocator benchmark evidence: ${output}"
