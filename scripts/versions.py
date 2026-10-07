#!/usr/bin/env python3
"""List Recall's released versions and the commands that go back to one.

    uv run scripts/versions.py            every version, newest first
    uv run scripts/versions.py 0.6.4      the commands for that version

This script only reads. It never changes the checkout or the installed
command; it prints the commands for a person to run. docs/ROLLBACK.md says
what each command does and what going back does not undo.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

VERSION = re.compile(r'(?m)^\[package\]\s*\n(?:(?!\[).*\n)*?^version\s*=\s*"([^"]+)"')


def git(root: Path, *args: str) -> str | None:
    """The output of a git command in `root`, or None when it fails."""
    try:
        done = subprocess.run(
            ["git", "-C", str(root), *args], capture_output=True, text=True, check=False
        )
    except OSError:
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def version_at(root: Path, commit: str) -> str | None:
    manifest = git(root, "show", f"{commit}:Cargo.toml")
    found = VERSION.search(manifest + "\n") if manifest else None
    return found.group(1) if found else None


def released_versions(root: Path, branch: str) -> list[dict[str, str]]:
    """One entry for each commit that changed the crate version, newest first."""
    log = git(root, "log", "--first-parent", "--format=%H%x09%cs%x09%s", branch, "--", "Cargo.toml")
    versions: list[dict[str, str]] = []
    for line in (log or "").splitlines():
        commit, date, subject = line.split("\t", 2)
        version = version_at(root, commit)
        if not version or version == version_at(root, f"{commit}~1"):
            continue  # this commit changed Cargo.toml but not the version
        versions.append({"version": version, "commit": commit, "date": date, "subject": subject})
    return versions


def installed_version() -> str | None:
    try:
        done = subprocess.run(["recall", "--version"], capture_output=True, text=True, check=False)
    except OSError:
        return None
    words = done.stdout.split()
    return words[-1] if done.returncode == 0 and words else None


def commands(root: Path, entry: dict[str, str]) -> str:
    short = entry["commit"][:7]
    return f"""To go back to {entry['version']} on this Mac:

    cd {root}
    git status --short          # must print nothing; commit or set aside work first
    git switch --detach {short}
    swift build --package-path capture-helper
    cargo install --path . --locked
    recall --version            # should print: recall {entry['version']}

To return to the newest version afterwards:

    cd {root}
    git switch main
    recall update

Recorded sessions are not touched by either step. docs/ROLLBACK.md has the rest."""


def main() -> int:
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel") or "")
    if not (root / "Cargo.toml").is_file():
        print("Run this inside the Recall checkout.")
        return 1
    branch = "origin/main" if git(root, "rev-parse", "--verify", "--quiet", "origin/main") else "HEAD"
    versions = released_versions(root, branch)
    if not versions:
        print(f"No versions found on {branch}.")
        return 1

    wanted = sys.argv[1] if len(sys.argv) > 1 else None
    if wanted:
        match = next((entry for entry in versions if entry["version"] == wanted), None)
        if not match:
            print(f"No version {wanted} on {branch}. Run with no argument to list them.")
            return 1
        print(commands(root, match))
        return 0

    installed = installed_version()
    head = git(root, "rev-parse", "HEAD") or ""
    print(f"Recall versions on {branch}, newest first:\n")
    for entry in versions:
        marks = []
        if entry["version"] == installed:
            marks.append("installed")
        if entry["commit"] == head:
            marks.append("checked out")
        note = f"  <- {', '.join(marks)}" if marks else ""
        print(f"  {entry['version']:<8} {entry['commit'][:7]}  {entry['date']}  {entry['subject']}{note}")
    if all(entry["commit"] != head for entry in versions):
        print(f"\nThis checkout is at {head[:7]}, which is not one of the commits above.")
    print("\nFor the commands that go back to one of them:")
    print(f"    uv run scripts/versions.py {versions[min(1, len(versions) - 1)]['version']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
