#!/usr/bin/env bash
# Warning-only: if src/ or capture-helper/ changed since the latest v* tag but
# Cargo.toml [package] version did not, print a reminder and exit 0.
# Prefer this over changing recall --version to version+commit (always-on noise);
# installed freshness already uses RECALL_BUILD_COMMIT.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

latest_tag="$(git tag -l 'v*' --sort=-v:refname | head -n 1 || true)"
if [[ -z "${latest_tag}" ]]; then
  echo "version freshness: no v* tags yet; skipping bump reminder."
  exit 0
fi

changed="$(git diff --name-only "${latest_tag}" -- src/ capture-helper/ || true)"
if [[ -z "${changed}" ]]; then
  echo "version freshness: no src/ or capture-helper/ changes since ${latest_tag}."
  exit 0
fi

version_at() {
  local rev="$1"
  git show "${rev}:Cargo.toml" 2>/dev/null \
    | awk '
      /^\[package\]/ { in_pkg=1; next }
      /^\[/ { in_pkg=0 }
      in_pkg && /^version[[:space:]]*=/ {
        gsub(/"/, "", $3)
        print $3
        exit
      }
    '
}

tag_version="$(version_at "${latest_tag}")"
head_version="$(version_at HEAD)"
if [[ -z "${tag_version}" || -z "${head_version}" ]]; then
  echo "version freshness: could not read Cargo.toml version; skipping."
  exit 0
fi

if [[ "${tag_version}" == "${head_version}" ]]; then
  echo "warning: src/ or capture-helper/ changed since ${latest_tag}, but Cargo.toml version is still ${head_version}."
  echo "warning: bump the crate version on user-facing pushes (see AGENTS.md Versioning)."
  exit 0
fi

echo "version freshness: Cargo.toml ${tag_version} -> ${head_version} since ${latest_tag}; ok."
exit 0
