#!/usr/bin/env python3
"""Fail when a pull-request body is not the Recall six-section shape.

Required headings, in order:

  ## TL;DR
  ## How it works
  ## Summary
  ## Files changed
  ## How to test
  ## Review

Also requires a ```mermaid fence. Fails on Cursor agent footer markers
(CURSOR_AGENT_PR_BODY, Open in Cursor / Open in Web), "Generated with",
and model names/ids. Bare product names (Claude, Cursor, Codex) are fine.

Usage:
  python3 scripts/check_pr_body.py [PATH]
  PR_BODY='...' python3 scripts/check_pr_body.py

Reads PATH when given, else the PR_BODY environment variable, else stdin.
Exit 0 when clean, 1 when problems are found, 2 on usage errors.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

REQUIRED_HEADINGS = [
    "TL;DR",
    "How it works",
    "Summary",
    "Files changed",
    "How to test",
    "Review",
]

_HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*$", re.M)
_MERMAID = re.compile(r"^```mermaid\s*$", re.M | re.I)

# Versioned / dotted model ids. Bare Claude / Cursor / Codex are not matched.
# Trailing file extensions are ignored so asset names are not treated as ids.
_MODEL_ID = re.compile(
    r"(?i)\b(?:"
    r"gpt-[\w.\-]+|"
    r"o[1-9](?:-[\w.\-]+)+|"
    r"claude-[\w.\-]+|"
    r"gemini-[\w.\-]+|"
    r"composer-[\w.\-]+|"
    r"grok-[\w.\-]+|"
    r"llama-[\w.\-]+|"
    r"mistral-[\w.\-]+|"
    r"codex-[\w.\-]+|"
    r"cursor-[\w.\-]+"
    r")\b"
)
_ASSET_SUFFIX = re.compile(
    r"\.(?:png|jpe?g|gif|webp|svg|ico|js|css|map|md|txt)$",
    re.I,
)

_GENERATED_WITH = re.compile(r"Generated with", re.I)
_OPEN_PHRASES = ("Open in Cursor", "Open in Web")


def check_body(text: str) -> list[str]:
    """Return problem lines for one PR body (empty when clean)."""
    errors: list[str] = []

    if "CURSOR_AGENT_PR_BODY" in text:
        errors.append(
            "CURSOR_AGENT_PR_BODY marker found. "
            "Fix: remove CURSOR_AGENT_PR_BODY HTML comment markers and the "
            "agent footer from the PR body."
        )

    open_hits = [phrase for phrase in _OPEN_PHRASES if phrase in text]
    if open_hits:
        shown = " / ".join(open_hits)
        errors.append(
            f"{shown} footer found. "
            "Fix: remove the Open in Cursor / Open in Web agent footer "
            "from the PR body."
        )

    if _GENERATED_WITH.search(text):
        errors.append(
            '"Generated with" attribution found. '
            'Fix: remove "Generated with ..." lines from the PR body.'
        )

    models = sorted(
        {m.group(0) for m in _MODEL_ID.finditer(text) if not _ASSET_SUFFIX.search(m.group(0))}
    )
    for model in models:
        errors.append(
            f"model id {model!r} found. "
            "Fix: remove model names/ids from the PR body; product names "
            "(Claude, Cursor, Codex) are fine."
        )

    found: list[str] = []
    for match in _HEADING.finditer(text):
        title = match.group(2).strip()
        if title in REQUIRED_HEADINGS:
            found.append(title)

    missing = [h for h in REQUIRED_HEADINGS if h not in found]
    if missing:
        errors.append(
            "missing required heading(s): "
            + ", ".join(missing)
            + ". Fix: add the six headings in order: "
            + ", ".join(REQUIRED_HEADINGS)
            + "."
        )
    else:
        # Required titles must appear in order (subsequence of found).
        want = list(REQUIRED_HEADINGS)
        pos = 0
        for title in found:
            if pos < len(want) and title == want[pos]:
                pos += 1
        if pos != len(want):
            errors.append(
                "required headings are out of order. "
                "Fix: use the six headings in order: " + ", ".join(REQUIRED_HEADINGS) + "."
            )

    if not _MERMAID.search(text):
        errors.append(
            "missing ```mermaid fence. Fix: add a ```mermaid code fence (under How it works)."
        )

    return errors


def _read_body(path: str | None) -> str:
    if path is not None:
        return Path(path).read_text(encoding="utf-8")
    env = os.environ.get("PR_BODY")
    if env is not None:
        return env
    if not sys.stdin.isatty():
        return sys.stdin.read()
    raise SystemExit("usage: check_pr_body.py [PATH]  (or set PR_BODY, or pipe stdin)")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "path",
        nargs="?",
        help="PR body file (default: PR_BODY env or stdin)",
    )
    args = parser.parse_args(argv)
    try:
        body = _read_body(args.path)
    except OSError as exc:
        print(f"pr-body: could not read input: {exc}", file=sys.stderr)
        return 2
    except SystemExit as exc:
        print(str(exc), file=sys.stderr)
        return 2

    problems = check_body(body)
    if not problems:
        print("pr-body: ok")
        return 0
    print(f"pr-body: {len(problems)} problem(s)", file=sys.stderr)
    for line in problems:
        print(f"error: {line}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
