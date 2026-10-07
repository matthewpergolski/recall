#!/usr/bin/env python3
"""Check the local memory bank for the signs of a stale note.

    uv run scripts/check_memory.py

A tool cannot tell whether a note is true. It can tell that a note is too
long, has no date, describes another commit, points at a file that is gone,
is missing from the index, or holds a secret. Run it at the start and at the
end of a session and fix what it reports. It only reads. The rules are in
AGENTS.md under "Memory Bank".
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

BANK = "memory-bank"
# The most lines a note about now may have. These are read every session or on
# need, so their size is paid for each time. Specs, benchmarks, the archive,
# and decisions.md are history and have no limit.
LIMITS = {
    "activeContext.md": 60,
    "projectbrief.md": 60,
    "techContext.md": 60,
    "tech/*.md": 120,
}
# The most characters in a line of activeContext. Without this, the line limit
# is met by packing more into a line.
ACTIVE_LINE = 120
CORE = ("activeContext.md", "projectbrief.md", "decisions.md", "techContext.md")
# Folders whose files the techContext index must name.
INDEXED = ("tech", "specs")
# Notes about now. A repo file named in one of these must exist.
CURRENT = ("activeContext.md", "projectbrief.md", "techContext.md", "tech/*.md")
PENDING = "memory handoff pending"

UPDATED = re.compile(r"Updated:? (\d{4}-\d{2}-\d{2})")
HEAD = re.compile(r"\bHEAD ([0-9a-f]{7,40})\b")
DECISION = re.compile(r"^## (?!\d{4}-\d{2}-\d{2}: )")
# A path inside the repo: folders, then a file name with an extension.
REPO_PATH = re.compile(
    r"(?<![\w./~-])((?:src|docs|tests|scripts|capture-helper|tech|specs|benchmarks|archive)"
    r"(?:/[\w.-]+)*/[\w.-]+\.[A-Za-z]\w*)"
)
SECRET = re.compile(
    r"(?i)\b[\w-]*(?:api[_-]?key|secret|token|password|passwd)[\w-]*\s*[=:]\s*[\"']?[A-Za-z0-9+/_\-]{16,}"
    r"|\b(?:sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|xox[abprs]-[A-Za-z0-9-]{20,})"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----"
)


def git(root: Path, *args: str) -> str | None:
    """The output of a git command in `root`, or None when it fails."""
    try:
        done = subprocess.run(
            ["git", "-C", str(root), *args], capture_output=True, text=True, check=False
        )
    except OSError:
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def read(path: Path) -> list[str]:
    return path.read_text(encoding="utf-8", errors="replace").splitlines()


def notes(bank: Path, patterns: tuple[str, ...] | dict[str, int]) -> list[Path]:
    return sorted(path for pattern in patterns for path in bank.glob(pattern))


def sizes(bank: Path) -> list[str]:
    found = []
    for pattern, limit in LIMITS.items():
        for path in sorted(bank.glob(pattern)):
            count = len(read(path))
            if count > limit:
                found.append(
                    f"{path}: {count} lines, the limit is {limit}. Cut what is no longer "
                    "true, or move it to where it is read on need."
                )
    active = bank / "activeContext.md"
    if active.exists():
        for number, line in enumerate(read(active), 1):
            if len(line) > ACTIVE_LINE:
                found.append(
                    f"{active}:{number}: {len(line)} characters, the limit is {ACTIVE_LINE}."
                )
    return found


def core_files(bank: Path) -> list[str]:
    return [f"{bank / name} is missing. It is a core note." for name in CORE if not (bank / name).exists()]


def status(root: Path, bank: Path) -> list[str]:
    """activeContext says when it was written and which commit it describes."""
    active = bank / "activeContext.md"
    if not active.exists():
        return []
    text = active.read_text(encoding="utf-8", errors="replace")
    found = []
    if PENDING in text.lower():
        found.append(f"{active}: a memory handoff is pending. Fold the other checkout's notes in.")
    if not UPDATED.search(text):
        found.append(f"{active}: no `Updated: YYYY-MM-DD` line.")
    named = HEAD.search(text)
    head = git(root, "rev-parse", "HEAD")
    if not named:
        found.append(f"{active}: names no commit. Write it as `HEAD abc1234`.")
    elif head and not head.startswith(named.group(1)):
        behind = git(root, "rev-list", "--count", f"{named.group(1)}..HEAD")
        detail = f"{behind} commit(s) behind HEAD" if behind else "not an ancestor of HEAD"
        found.append(
            f"{active}: describes HEAD {named.group(1)}, but the checkout is at {head[:7]} "
            f"({detail}). Read `git log` and rewrite the state."
        )
    return found


def decisions(bank: Path) -> list[str]:
    path = bank / "decisions.md"
    if not path.exists():
        return []
    return [
        f"{path}:{number}: a decision heading must start `## YYYY-MM-DD: `."
        for number, line in enumerate(read(path), 1)
        if DECISION.match(line)
    ]


def index(bank: Path) -> list[str]:
    """The techContext index names every subject and spec, and nothing that is gone."""
    path = bank / "techContext.md"
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8", errors="replace")
    found = []
    for folder in INDEXED:
        on_disk = {f"{folder}/{item.name}" for item in (bank / folder).glob("*.md")}
        named = set(re.findall(rf"\b{folder}/[\w.-]+\.md\b", text))
        found += [f"{path}: does not name {name}." for name in sorted(on_disk - named)]
        found += [f"{path}: names {name}, which does not exist." for name in sorted(named - on_disk)]
    return found


def paths(root: Path, bank: Path) -> list[str]:
    found = []
    for note in notes(bank, CURRENT):
        for number, line in enumerate(read(note), 1):
            for name in REPO_PATH.findall(line):
                if not (root / name).exists() and not (bank / name).exists():
                    found.append(f"{note}:{number}: `{name}` does not exist.")
    return found


def secrets(bank: Path) -> list[str]:
    found = []
    for note in sorted(bank.rglob("*.md")):
        for number, line in enumerate(read(note), 1):
            if SECRET.search(line):
                found.append(f"{note}:{number}: looks like a secret. Remove it.")
    return found


def main() -> int:
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel") or Path.cwd())
    bank = root / BANK
    if not bank.is_dir():
        print(f"No {BANK}/ in {root}. Nothing to check; AGENTS.md says how to start one.")
        return 0
    found = (
        core_files(bank)
        + sizes(bank)
        + status(root, bank)
        + decisions(bank)
        + index(bank)
        + paths(root, bank)
        + secrets(bank)
    )
    for line in found:
        print(line.replace(f"{root}/", ""))
    print(f"memory bank: {len(found)} to fix" if found else "memory bank: nothing to fix")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
