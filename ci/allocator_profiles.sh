#!/usr/bin/env bash
# Diagnostic-only pprof and exact-DHAT collection; never use these runs as timing evidence.
set -euo pipefail

candidate_revision=e08686773514edf0b30a88e5950a6071d3caf373
candidate_tree=7162acc8005911d79aa85397a20a6744d27a09b8
candidate_archive_sha256=53ba911ffc1ffe65e806c47b066064cae76ab461d97a156b4a4e86fa37f3211f
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="${root}/target/allocator-profiles"
build_dir="${state}/build"
binary_dir="${state}/bin"
output="${RELD_ALLOCATOR_PROFILE_OUTPUT:-${state}/results.json}"
source_archive="${RELD_ALLOCATOR_SOURCE_ARCHIVE:-${state}/candidate-e0868677.tar}"

test "$(git -C "${root}" rev-parse "${candidate_revision}^{tree}")" = "${candidate_tree}"
if [[ ! -f "${source_archive}" ]]; then
  mkdir -p "$(dirname "${source_archive}")"
  git -C "${root}" archive --format=tar --output="${source_archive}" "${candidate_revision}"
fi
echo "${candidate_archive_sha256}  ${source_archive}" | sha256sum --check --status
mkdir -p "${state}" "${binary_dir}"
source_dir=
trap 'python3 "${root}/ci/allocator_source_cleanup.py" "${source_dir}"' EXIT
source_dir="$(mktemp -d "${state}/source-e0868677.XXXXXXXX")"
tar -xf "${source_archive}" -C "${source_dir}"

common_env=(
  env -i
  "PATH=${RELD_PINNED_CARGO_BIN:-/usr/local/cargo/bin}:/usr/bin:/bin"
  "HOME=/tmp/reld-allocator-profile-home"
  "CARGO_HOME=${state}/cargo-home"
  "RUSTUP_HOME=${RELD_PINNED_RUSTUP_HOME:-/usr/local/rustup}"
  "RUSTUP_TOOLCHAIN=1.95.0"
  "CC=/usr/bin/clang"
  "CXX=/usr/bin/clang++"
  "SOURCE_DATE_EPOCH=0"
)
mkdir -p /tmp/reld-allocator-profile-home
mkdir -p "${state}/cargo-home"
test "$(command -v cargo)" = "${RELD_PINNED_CARGO_BIN:-/usr/local/cargo/bin}/cargo"
test -x "${RELD_PINNED_RUSTUP_HOME:-/usr/local/rustup}/toolchains/1.95.0-x86_64-unknown-linux-gnu/bin/rustc"

for mode in default pprof dhat; do
  features=fork,plugins,zstd
  if [[ "${mode}" == pprof ]]; then
    features="${features},mimalloc-pprof-profile"
  elif [[ "${mode}" == dhat ]]; then
    features="${features},mimalloc-pprof-dhat"
  fi
  "${common_env[@]}" CARGO_TARGET_DIR="${build_dir}" cargo build --locked --release --no-default-features --features "${features}" --manifest-path "${source_dir}/Cargo.toml" -p reld --bin reld
  install -m 755 "${build_dir}/release/reld" "${binary_dir}/reld-${mode}"
done

"${common_env[@]}" python3 -m ci.allocator_profile_runner \
  --default "${binary_dir}/reld-default" \
  --pprof "${binary_dir}/reld-pprof" \
  --dhat "${binary_dir}/reld-dhat" \
  --output "${output}" \
  --output-dir "${state}/evidence" \
  --manifest "${source_dir}/ci/e2e/link-workload/Cargo.toml"

echo "diagnostic allocator profiles: ${output}"
