#!/usr/bin/env bash
# Point this checkout at the committed hooks so commit and push run the checks.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
current="$(git config --get core.hooksPath || true)"
if [[ -n "${current}" && "${current}" != ".githooks" ]]; then
  echo "hooks: core.hooksPath is ${current}, not .githooks. Recall hooks were not installed." >&2
  exit 1
fi
git config core.hooksPath .githooks
echo "hooks: core.hooksPath=.githooks"
