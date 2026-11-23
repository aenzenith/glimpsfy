#!/usr/bin/env bash
# CI's checks, in /work. Every step runs even when an earlier one fails, so one pass lists
# everything Linux trips over. No production build.
set -uo pipefail

failed=()
step() {
  local name=$1
  shift
  printf '\n==> %s\n' "$name"
  if "$@"; then
    printf '<== %s: ok\n' "$name"
  else
    printf '<== %s: FAILED\n' "$name"
    failed+=("$name")
    return 1
  fi
}

if step "npm ci" npm ci --no-audit --no-fund; then
  step "vue-tsc" npm run typecheck
  step "vitest" npm test
fi
step "cargo fmt" cargo fmt --all --check
step "clippy" cargo clippy --workspace --all-targets -- -D warnings
step "cargo test" cargo test --workspace

echo
if ((${#failed[@]})); then
  echo "Linux check failed: ${failed[*]}"
  exit 1
fi
echo "Linux check passed"
