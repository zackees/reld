#!/usr/bin/env bash
set -euo pipefail

profile="${1:-debug}"
target_triple="${2:-}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
if [[ "${target_dir}" != /* ]]; then
  target_dir="${repo_root}/${target_dir}"
fi
if [[ -n "${target_triple}" ]]; then
  bin_dir="${target_dir}/${target_triple}/${profile}"
else
  bin_dir="${target_dir}/${profile}"
fi
source_bin="${bin_dir}/reld"

test -x "${source_bin}"
cp -f "${source_bin}" "${bin_dir}/ld.reld"
cp -f "${source_bin}" "${bin_dir}/ld64.reld"
