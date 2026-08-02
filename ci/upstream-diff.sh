#!/usr/bin/env bash
set -euo pipefail

upstream_url="https://github.com/wild-linker/wild.git"
upstream_sha="5793935f1d8b05b9a978ce2089e16e718072e9a9"
mode="full"

if [[ "${1:-}" == "--stat" ]]; then
  mode="stat"
  shift
fi
if [[ $# -gt 0 ]]; then
  upstream_sha="$1"
  shift
fi
if [[ $# -ne 0 ]]; then
  echo "usage: $0 [--stat] [upstream-sha]" >&2
  exit 64
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

git -C "${tmp_dir}" init -q
git -C "${tmp_dir}" remote add origin "${upstream_url}"
git -C "${tmp_dir}" fetch -q --depth 1 origin "${upstream_sha}"
git -C "${tmp_dir}" checkout -q --detach FETCH_HEAD

set +e
if [[ "${mode}" == "stat" ]]; then
  git diff --no-index --stat -- "${tmp_dir}/libwild/src" "${repo_root}/crates/reld-core/src"
else
  git diff --no-index -- "${tmp_dir}/libwild/src" "${repo_root}/crates/reld-core/src"
fi
status=$?
set -e

if [[ ${status} -gt 1 ]]; then
  exit "${status}"
fi
