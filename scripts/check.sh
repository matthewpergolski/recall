#!/usr/bin/env bash
# One entry point for Recall verification. Fail fast on the first hard failure.
# Version freshness is warning-only: RECALL_BUILD_COMMIT already tracks installed
# freshness, so rewriting --version would be always-on noise; this only warns
# when a crate bump was likely forgotten after src/ or capture-helper/ changes.
#
# Optional memory: run `cairnkit check` only when cairnkit is on PATH and
# `memory status` shows this checkout has a cairnkit memory folder (outside
# the repo). Legacy in-repo memory-bank/ alone is not enough. If unclear,
# skip silently — see scripts/optional_cairnkit_check.sh.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

run() {
  echo "==> $*"
  "$@"
}

run cargo fmt --check
run cargo check
run cargo test
run cargo clippy -- -D warnings

echo "==> scripts/optional_cairnkit_check.sh"
bash scripts/optional_cairnkit_check.sh

echo "==> scripts/check_version_freshness.sh (warning-only)"
bash scripts/check_version_freshness.sh

echo "==> scripts/test_commit_msg_hook.sh"
bash scripts/test_commit_msg_hook.sh

echo "==> scripts/test_cairnkit_check_gate.sh"
bash scripts/test_cairnkit_check_gate.sh

echo "==> python3 -m unittest discover (check_identity / check_pr_body)"
python3 -m unittest discover -s tests -p 'test_check_*.py' -q

echo "All checks passed."
