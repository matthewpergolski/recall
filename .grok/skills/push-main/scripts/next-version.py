#!/usr/bin/env python3
"""Evaluate Recall's next crate version from git, not memory.

Source of truth for "what already shipped" is Cargo.toml on origin/main.
Working-tree Cargo.toml is what this checkout is about to commit.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

VERSION_RE = re.compile(
    r'(?m)^\[package\]\s*\n(?:(?!\[).*\n)*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"',
)


def run(repo: Path, *args: str) -> str:
    result = subprocess.run(
        args,
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def parse_version(toml: str) -> tuple[int, int, int]:
    match = VERSION_RE.search(toml)
    if not match:
        raise SystemExit("could not parse package.version from Cargo.toml")
    return int(match.group(1)), int(match.group(2)), int(match.group(3))


def fmt(version: tuple[int, int, int]) -> str:
    return f"{version[0]}.{version[1]}.{version[2]}"


def bump_feature(version: tuple[int, int, int]) -> tuple[int, int, int]:
    major, minor, _patch = version
    return major, minor + 1, 0


def bump_patch(version: tuple[int, int, int]) -> tuple[int, int, int]:
    major, minor, patch = version
    return major, minor, patch + 1


def repo_root(start: Path | None) -> Path:
    if start is None:
        start = Path.cwd()
    return Path(
        run(start, "git", "rev-parse", "--show-toplevel")
    )


def read_toml(repo: Path, spec: str) -> str:
    return run(repo, "git", "show", spec)


def last_bump(repo: Path, ref: str) -> dict[str, str] | None:
    log = run(
        repo,
        "git",
        "log",
        "-1",
        "--format=%H%x09%s",
        "-G",
        'version = "',
        ref,
        "--",
        "Cargo.toml",
    )
    if not log:
        return None
    commit, _, subject = log.partition("\t")
    version = fmt(parse_version(read_toml(repo, f"{commit}:Cargo.toml")))
    return {"commit": commit, "subject": subject, "version": version}


def classify_status(
    kind: str,
    released: tuple[int, int, int],
    working: tuple[int, int, int],
    proposed: tuple[int, int, int],
) -> str:
    if kind == "none":
        return "ok_no_bump" if working == released else "mismatch"
    if working == proposed:
        return "already_bumped"
    if working == released:
        return "needs_bump"
    return "mismatch"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=None)
    parser.add_argument(
        "--kind",
        choices=("feature", "patch", "none"),
        default=None,
        help="feature = minor bump, patch = patch bump, none = do not bump",
    )
    parser.add_argument("--ref", default="origin/main")
    args = parser.parse_args()

    repo = repo_root(args.repo)
    try:
        released_toml = read_toml(repo, f"{args.ref}:Cargo.toml")
    except subprocess.CalledProcessError:
        print(
            f"missing {args.ref}:Cargo.toml; fetch origin first",
            file=sys.stderr,
        )
        return 2

    working_toml = (repo / "Cargo.toml").read_text()
    released = parse_version(released_toml)
    working = parse_version(working_toml)
    next_feature = bump_feature(released)
    next_patch = bump_patch(released)

    proposed = {
        "feature": next_feature,
        "patch": next_patch,
        "none": released,
        None: None,
    }[args.kind]

    payload = {
        "repo": str(repo),
        "branch": run(repo, "git", "branch", "--show-current"),
        "ref": args.ref,
        "released": fmt(released),
        "released_commit": run(repo, "git", "rev-parse", "--short", args.ref),
        "working": fmt(working),
        "last_bump": last_bump(repo, args.ref),
        "next_feature": fmt(next_feature),
        "next_patch": fmt(next_patch),
        "kind": args.kind,
        "proposed": None if proposed is None else fmt(proposed),
        "status": None
        if proposed is None
        else classify_status(args.kind, released, working, proposed),
    }
    json.dump(payload, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
