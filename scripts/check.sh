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
unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_OBJECT_DIRECTORY GIT_COMMON_DIR

echo "==> scripts/install-git-hooks.sh"
bash scripts/install-git-hooks.sh

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

echo "==> python3 scripts/check_identity.py origin/main..HEAD"
python3 scripts/check_identity.py origin/main..HEAD

echo "==> python3 scripts/check_version_bump.py --base origin/main --head HEAD"
python3 scripts/check_version_bump.py --base origin/main --head HEAD

# Optional local PR-body gate: pass a file path or the PR_BODY env var.
# --head is this checkout. CI passes the pull request head SHA instead.
head_sha="$(git rev-parse HEAD)"
if [[ -n "${PR_BODY_FILE:-}" ]]; then
  echo "==> python3 scripts/check_pr_body.py ${PR_BODY_FILE} --head ${head_sha}"
  python3 scripts/check_pr_body.py "${PR_BODY_FILE}" --head "${head_sha}"
elif [[ -n "${PR_BODY:-}" ]]; then
  echo "==> python3 scripts/check_pr_body.py --head ${head_sha}"
  python3 scripts/check_pr_body.py --head "${head_sha}"
fi

echo "All checks passed."
