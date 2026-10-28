#!/usr/bin/env bash
# Optional cairnkit gate for scripts/check.sh / pre-push.
#
# Run `cairnkit check` only when:
#   1. `cairnkit` is on PATH, and
#   2. this checkout has a cairnkit memory folder (outside the repo).
#
# Detect (2) with `memory status` exiting 0. Do not treat legacy in-repo
# `memory-bank/` as cairnkit setup (`memory migrate` folds that elsewhere).
# If cairnkit/memory is missing or status is unclear, skip silently (exit 0).
set -euo pipefail

if ! command -v cairnkit >/dev/null 2>&1; then
  exit 0
fi

if ! command -v memory >/dev/null 2>&1; then
  exit 0
fi

# `memory status` succeeds only when a memory folder is configured for this checkout.
if ! memory status >/dev/null 2>&1; then
  exit 0
fi

echo "==> cairnkit check"
exec cairnkit check
