#!/usr/bin/env python3
"""Fail unless Cargo.toml is unchanged or an exact patch bump from the base tip.

A change under src/ requires that patch bump. Cargo.lock must name the same
recall version. semver:minor or semver:major allows the bigger exact bump.
Matthew applies those labels. CI passes them with --labels or --event-file.
A local push can set RECALL_SEMVER_LABEL. CI ignores that variable when
--labels or --event-file is set.

Usage:
  python3 scripts/check_version_bump.py [--base origin/main] [--head HEAD]
  python3 scripts/check_version_bump.py --staged
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

_VERSION = re.compile(
    r'(?m)^\[package\]\s*\n(?:(?!\[).*\n)*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"'
)
_PR_NUMBER = re.compile(r"#(\d+)")


def _run(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    for key in (
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ):
        env.pop(key, None)
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )


def parse_version(toml: str) -> tuple[int, int, int] | None:
    match = _VERSION.search(toml)
    if not match:
        return None
    return int(match.group(1)), int(match.group(2)), int(match.group(3))


def fmt(version: tuple[int, int, int]) -> str:
    return f"{version[0]}.{version[1]}.{version[2]}"


def lock_version(text: str) -> str | None:
    found: list[str] = []
    for block in re.split(r"(?m)^\[\[package\]\]\s*$", text)[1:]:
        name = re.search(r'(?m)^name = "([^"]+)"', block)
        version = re.search(r'(?m)^version = "([^"]+)"', block)
        if name and version and name.group(1) == "recall":
            found.append(version.group(1))
    if len(found) == 1:
        return found[0]
    return None


def allowed_bumps(base: tuple[int, int, int], labels: set[str]) -> set[tuple[int, int, int]]:
    major, minor, patch = base
    allowed = {(major, minor, patch + 1)}
    if "semver:minor" in labels or "semver:major" in labels:
        allowed.add((major, minor + 1, 0))
    if "semver:major" in labels:
        allowed.add((major + 1, 0, 0))
    return allowed


def next_version(base: tuple[int, int, int], labels: set[str]) -> tuple[int, int, int]:
    """The next version the labels allow. A label selects that bump, not a patch."""
    major, minor, _patch = base
    if "semver:major" in labels:
        return major + 1, 0, 0
    if "semver:minor" in labels:
        return major, minor + 1, 0
    return major, minor, base[2] + 1


def bump_problems(
    base: tuple[int, int, int],
    head: tuple[int, int, int],
    labels: set[str],
    src_changed: bool,
    version_moved: bool = False,
) -> list[str]:
    """Judge `head` against the current base tip, not an older merge-base.

    `version_moved` is true when this branch changed Cargo.toml since it split
    from the base. Two branches can both pick the same version. The second one
    then matches the base tip and must take the next allowed version.
    """
    nxt = fmt(next_version(base, labels))
    if not version_moved and not src_changed:
        return []
    if head in allowed_bumps(base, labels):
        return []
    if head == base and src_changed and not version_moved:
        return [
            "version/src-bump: src/ changed but the current base is "
            f"{fmt(base)}. Set Cargo.toml and Cargo.lock to {nxt}."
        ]
    if head == base:
        return [
            f"version/patch: the current base is already {fmt(base)}. "
            f"Set Cargo.toml and Cargo.lock to {nxt}."
        ]
    return [
        f"version/patch: the current base is {fmt(base)}. "
        f"Cargo.toml is {fmt(head)}. Set Cargo.toml and Cargo.lock to {nxt}. "
        "A bigger bump needs the semver:minor or semver:major label, which Matthew applies."
    ]


def parse_label_text(raw: str | None) -> set[str]:
    if not raw:
        return set()
    return {part.strip() for part in raw.split(",") if part.strip()}


def labels_from_event(event: dict, repo: str | None, token: str | None) -> set[str]:
    """Labels on the pull_request payload, or labels of a PR named in a push."""
    pull = event.get("pull_request")
    if isinstance(pull, dict):
        names = []
        for label in pull.get("labels") or []:
            if isinstance(label, dict) and label.get("name"):
                names.append(str(label["name"]))
        return set(names)
    texts = []
    for commit in event.get("commits") or []:
        if isinstance(commit, dict):
            texts.append(str(commit.get("message") or ""))
    head_commit = event.get("head_commit")
    if isinstance(head_commit, dict):
        texts.append(str(head_commit.get("message") or ""))
    numbers: list[str] = []
    for text in texts:
        numbers.extend(_PR_NUMBER.findall(text))
    found: set[str] = set()
    if not numbers or not repo or not token:
        return found
    for number in dict.fromkeys(numbers):
        found |= _fetch_labels(repo, number, token)
    return found


def _fetch_labels(repo: str, number: str, token: str) -> set[str]:
    url = f"https://api.github.com/repos/{repo}/issues/{number}/labels"
    request = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "recall-version-bump",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = response.read().decode("utf-8")
    except (urllib.error.URLError, TimeoutError, OSError):
        return set()
    try:
        labels = json.loads(payload)
    except json.JSONDecodeError:
        return set()
    if not isinstance(labels, list):
        return set()
    return {str(item["name"]) for item in labels if isinstance(item, dict) and item.get("name")}


def _show(repo: Path, spec: str) -> str | None:
    result = _run(repo, "show", spec)
    if result.returncode != 0:
        return None
    return result.stdout


def _src_diff(repo: Path, base_sha: str, head_sha: str) -> tuple[bool, str | None]:
    """Return (src changed, error). A git failure is an error, not 'no change'."""
    result = _run(repo, "diff", "--name-only", base_sha, head_sha, "--", "src")
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        return False, f"version-bump: git diff failed for src/: {detail}"
    return bool(result.stdout.strip()), None


def check_range(repo: Path, base: str, head: str, labels: set[str]) -> tuple[list[str], int]:
    merged = _run(repo, "merge-base", base, head)
    if merged.returncode != 0:
        detail = (merged.stderr or merged.stdout).strip()
        return [f"version-bump: git merge-base {base} {head} failed: {detail}"], 2
    merge_sha = merged.stdout.strip()
    tip = _run(repo, "rev-parse", "--verify", base)
    if tip.returncode != 0:
        detail = (tip.stderr or tip.stdout).strip()
        return [f"version-bump: could not resolve base {base}: {detail}"], 2
    tip_sha = tip.stdout.strip()
    tip_toml = _show(repo, f"{tip_sha}:Cargo.toml")
    merge_toml = _show(repo, f"{merge_sha}:Cargo.toml")
    head_toml = _show(repo, f"{head}:Cargo.toml")
    head_lock = _show(repo, f"{head}:Cargo.lock")
    if tip_toml is None or merge_toml is None or head_toml is None:
        return ["version-bump: could not read Cargo.toml at the base or the head"], 2
    tip_version = parse_version(tip_toml)
    merge_version = parse_version(merge_toml)
    head_version = parse_version(head_toml)
    if tip_version is None or merge_version is None or head_version is None:
        return ["version-bump: could not parse [package] version in Cargo.toml"], 2
    changed, diff_error = _src_diff(repo, merge_sha, head)
    if diff_error:
        return [diff_error], 2
    problems = bump_problems(
        tip_version,
        head_version,
        labels,
        changed,
        head_version != merge_version,
    )
    if head_lock is None:
        problems.append("version/lock: Cargo.lock is missing at the head. Fix: commit Cargo.lock.")
    else:
        locked = lock_version(head_lock)
        if locked != fmt(head_version):
            problems.append(
                "version/lock: Cargo.lock recall version "
                f"{locked or '(missing)'} does not match Cargo.toml {fmt(head_version)}. "
                "Fix: set both to the same version."
            )
    return problems, 1 if problems else 0


def check_staged(repo: Path) -> tuple[list[str], int]:
    diff = _run(repo, "diff", "--cached", "--name-only", "--", "Cargo.toml", "Cargo.lock")
    if diff.returncode != 0:
        detail = (diff.stderr or diff.stdout).strip()
        return [f"version-bump: git diff --cached failed: {detail}"], 2
    if not diff.stdout.strip():
        return [], 0
    toml = _show(repo, ":Cargo.toml")
    lock = _show(repo, ":Cargo.lock")
    if toml is None or lock is None:
        return ["version/lock: stage both Cargo.toml and Cargo.lock"], 1
    version = parse_version(toml)
    if version is None:
        return ["version-bump: could not parse the staged Cargo.toml version"], 2
    locked = lock_version(lock)
    if locked != fmt(version):
        return [
            "version/lock: staged Cargo.lock recall version "
            f"{locked or '(missing)'} does not match staged Cargo.toml {fmt(version)}. "
            "Fix: stage both files with the same version."
        ], 1
    return [], 0


def _labels(args: argparse.Namespace) -> set[str]:
    if args.event_file:
        try:
            event = json.loads(Path(args.event_file).read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise SystemExit(f"version-bump: could not read event file: {exc}") from exc
        if not isinstance(event, dict):
            raise SystemExit("version-bump: event file must be a JSON object")
        return labels_from_event(
            event,
            os.environ.get("GITHUB_REPOSITORY"),
            os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN"),
        )
    if args.labels is None:
        return parse_label_text(os.environ.get("RECALL_SEMVER_LABEL"))
    return parse_label_text(args.labels)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=".", help="git repository (default: .)")
    parser.add_argument("--base", default="origin/main", help="base ref (default: origin/main)")
    parser.add_argument("--head", default="HEAD", help="head ref (default: HEAD)")
    parser.add_argument(
        "--labels",
        default=None,
        help="comma-separated labels. Empty string means no labels. "
        "Omit to read RECALL_SEMVER_LABEL.",
    )
    parser.add_argument(
        "--event-file",
        help="GitHub event JSON. Labels come from this file, not from the environment.",
    )
    parser.add_argument(
        "--staged",
        action="store_true",
        help="check the index only: Cargo.toml and Cargo.lock must agree",
    )
    args = parser.parse_args(argv)
    repo = Path(args.repo)
    try:
        labels = set() if args.staged else _labels(args)
    except SystemExit as exc:
        print(str(exc), file=sys.stderr)
        return 2
    if args.staged:
        problems, code = check_staged(repo)
    else:
        problems, code = check_range(repo, args.base, args.head, labels)
    if code == 0 and not problems:
        print("version-bump: ok")
        return 0
    print(f"version-bump: {len(problems)} problem(s)", file=sys.stderr)
    for line in problems:
        print(f"error: {line}", file=sys.stderr)
    return code if code else 1


if __name__ == "__main__":
    sys.exit(main())
