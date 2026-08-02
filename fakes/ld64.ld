#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -x "${repo_root}/target/release/reld" ]]; then
  reld="${repo_root}/target/release/reld"
else
  reld="${repo_root}/target/debug/reld"
fi
exec "${reld}" -flavor darwin "$@"
