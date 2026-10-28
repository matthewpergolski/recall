#!/usr/bin/env bash
# Fail if .githooks/commit-msg accepts a denylisted message or rejects a clean one.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
hook="${root}/.githooks/commit-msg"
tmp="$(mktemp)"
trap 'rm -f "${tmp}"' EXIT

expect_reject() {
  local label="$1"
  local body="$2"
  printf '%s\n' "${body}" >"${tmp}"
  if bash "${hook}" "${tmp}" >/dev/null 2>&1; then
    echo "FAIL: commit-msg accepted denylisted message (${label})" >&2
    exit 1
  fi
}

expect_accept() {
  local label="$1"
  local body="$2"
  printf '%s\n' "${body}" >"${tmp}"
  if ! bash "${hook}" "${tmp}" >/dev/null 2>&1; then
    echo "FAIL: commit-msg rejected clean message (${label})" >&2
    exit 1
  fi
}

expect_reject "generated-with" "Fix bug

Generated with Claude"
expect_reject "co-authored-claude" "Fix bug

Co-Authored-By: Claude <noreply@anthropic.com>"
expect_reject "co-authored-cursor-agent" "Fix bug

Co-Authored-By: Cursor Agent <agent@cursor.com>"
expect_reject "co-authored-cursoragent" "Fix bug

Co-Authored-By: cursoragent <cursoragent@cursor.com>"
expect_reject "co-authored-bare-cursor" "Fix bug

Co-Authored-By: Cursor <cursoragent@cursor.com>"
expect_reject "co-authored-codex" "Fix bug

Co-Authored-By: Codex <codex@openai.com>"

expect_accept "plain" "Add scripts/check.sh entry point"
expect_accept "human-coauthor" "Fix docs

Co-Authored-By: Ada Lovelace <ada@example.com>"

echo "commit-msg hook self-check passed."
