#!/usr/bin/env python3
"""Fail when a pull-request body is not the Recall six-section shape.

Required headings, in order:

  ## TL;DR
  ## How it works
  ## Summary
  ## Files changed
  ## How to test
  ## Review

Also requires a ```mermaid fence, bare Implementer and Reviewer names,
an End-to-end command and result, and a Head SHA. Fails on Cursor agent
footer markers and versioned model ids.

Usage:
  python3 scripts/check_pr_body.py [PATH] --head SHA
  python3 scripts/check_pr_body.py PATH --fix --head SHA
  python3 scripts/check_pr_body.py --github --head SHA
  scripts/pr_body_check PATH --head SHA

--fix keeps only the text between CURSOR_AGENT_PR_BODY markers and drops
the footer. It does not write when the body is already clean.
--github does that to the open pull request, then validates the result.
Exit 0 when clean, 1 when problems are found, 2 on usage errors.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from pr_body_rules import (  # noqa: E402
    REQUIRED_HEADINGS,
    check_body,
    explain_github_denied,
    plan_body_update,
    should_write_body,
)


def _read_body(path: str | None) -> str:
    if path is not None:
        return Path(path).read_text(encoding="utf-8")
    env = os.environ.get("PR_BODY")
    if env is not None:
        return env
    if not sys.stdin.isatty():
        return sys.stdin.read()
    raise SystemExit("usage: check_pr_body.py [PATH] --head SHA  (or set PR_BODY, or pipe stdin)")


def _event_action() -> str:
    path = os.environ.get("GITHUB_EVENT_PATH")
    if not path:
        return ""
    try:
        payload = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return ""
    action = payload.get("action")
    return action if isinstance(action, str) else ""


def _github_request(method: str, url: str, token: str, payload: dict | None = None) -> tuple[int, str]:
    data = None
    headers = {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "recall-pr-body-check",
    }
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode("utf-8", errors="replace")
        return exc.code, raw


def _report(problems: list[str]) -> int:
    if not problems:
        print("pr-body: ok")
        return 0
    print(f"pr-body: {len(problems)} problem(s)", file=sys.stderr)
    for line in problems:
        print(f"error: {line}", file=sys.stderr)
    return 1


def _run_github(head: str | None) -> int:
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN") or ""
    repo = os.environ.get("GITHUB_REPOSITORY") or ""
    number = os.environ.get("PR_NUMBER") or ""
    if not token or not repo or not number:
        print(
            "pr-body: --github needs GITHUB_TOKEN, GITHUB_REPOSITORY, and PR_NUMBER",
            file=sys.stderr,
        )
        return 2
    url = f"https://api.github.com/repos/{repo}/pulls/{number}"
    status, payload = _github_request("GET", url, token)
    if status >= 300:
        print(f"pr-body: could not read PR #{number}: {status} {payload}", file=sys.stderr)
        return 1
    try:
        pr = json.loads(payload)
    except json.JSONDecodeError:
        print("pr-body: GitHub returned a body that is not JSON", file=sys.stderr)
        return 1
    body = pr.get("body") or ""
    if head is None:
        head = (pr.get("head") or {}).get("sha")
    updated = should_write_body(
        body,
        actor=os.environ.get("GITHUB_ACTOR", ""),
        action=_event_action(),
    )
    if updated is not None:
        status, payload = _github_request("PATCH", url, token, {"body": updated})
        if status in (401, 403):
            print(explain_github_denied(status, payload), file=sys.stderr)
            problems = check_body(body, head=head)
            if not problems:
                problems = [
                    "pr-body: the body still needs a GitHub edit and the token cannot write."
                ]
            return _report(problems)
        if status >= 300:
            print(f"pr-body: could not update PR #{number}: {status} {payload}", file=sys.stderr)
            return 1
        print("pr-body: removed the Cursor wrapper")
        body = updated
    return _report(check_body(body, head=head))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", nargs="?", help="PR body file (default: PR_BODY env or stdin)")
    parser.add_argument("--head", help="PR head SHA. The Head: line must equal it.")
    parser.add_argument(
        "--fix",
        action="store_true",
        help="keep only the text between Cursor markers and rewrite the file",
    )
    parser.add_argument(
        "--github",
        action="store_true",
        help="strip the open PR body through the API, then validate it",
    )
    args = parser.parse_args(argv)

    if args.github:
        return _run_github(args.head)
    if args.fix and not args.path:
        print("pr-body: --fix needs a file path", file=sys.stderr)
        return 2
    try:
        body = _read_body(args.path)
    except OSError as exc:
        print(f"pr-body: could not read input: {exc}", file=sys.stderr)
        return 2
    except SystemExit as exc:
        print(str(exc), file=sys.stderr)
        return 2

    if args.fix and args.path:
        updated = plan_body_update(body)
        if updated is not None:
            Path(args.path).write_text(updated, encoding="utf-8")
            body = updated
            print("pr-body: removed the Cursor wrapper")
    return _report(check_body(body, head=args.head))


if __name__ == "__main__":
    sys.exit(main())
