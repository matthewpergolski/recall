#!/usr/bin/env bash
# Fail when install docs lose the curl one-liner or keep the old discovery sentence.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
curl_line='curl -fsSL https://raw.githubusercontent.com/matthewpergolski/recall/main/scripts/install.sh | sh'
old_discovery='uses the checkout embedded in the installed binary, then falls back'
fail=0

for file in README.md docs/SETUP.md docs/PORTABILITY.md; do
  path="$root/$file"
  if ! grep -Fq "$curl_line" "$path"; then
    echo "install-docs: $file is missing the curl install line" >&2
    fail=1
  fi
done

if grep -Fq "$old_discovery" "$root/docs/SETUP.md"; then
  echo "install-docs: docs/SETUP.md still contains the old embedded-checkout discovery sentence" >&2
  fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "install-docs: ok"
