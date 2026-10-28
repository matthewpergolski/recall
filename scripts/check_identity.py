#!/usr/bin/env python3
"""Fail when a commit range is not under the neutral Recall identity.

Allowed identity (author and committer):

  recall contributors <contributors@recall.invalid>

Fails when a commit has a Co-Authored-By, Reviewed-by, or Claude trailer,
or an AI model name/id in the author, committer, or message. Signed-off-by
must use the neutral identity when present.

Also fails when a commit's added lines introduce a personal-looking email
address other than that neutral address. Generic placeholders such as
git@github.com and *@example.* are allowed in file text. Paths under
tests/ are skipped for the file-text scan so fixtures may use synthetic
addresses; author, committer, and trailers are still checked on every commit.

Usage:
  python3 scripts/check_identity.py [BASE..HEAD]
  python3 scripts/check_identity.py origin/main..HEAD

Exit 0 when clean, 1 when problems are found, 2 on usage/git errors.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ALLOWED_NAME = "recall contributors"
ALLOWED_EMAIL = "contributors@recall.invalid"
ALLOWED = f"{ALLOWED_NAME} <{ALLOWED_EMAIL}>"

# Placeholders that docs and tests may show. Not a personal inbox.
_GENERIC_EMAIL = re.compile(
    r"^(?:"
    r"git@github\.com|"
    r"[A-Za-z0-9._%+\-]+@example\.(?:com|org|net|invalid)"
    r")$",
    re.I,
)
_EMAIL = re.compile(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b")

# Trailers that must never appear (any value).
_FORBIDDEN_TRAILER = re.compile(
    r"^(Co-authored-by|Reviewed-by|Claude(?:-[A-Za-z0-9]+)?)\s*:",
    re.I | re.M,
)
# Signed-off-by may exist only with the neutral identity.
_SIGNED_OFF = re.compile(
    r"^Signed-off-by:\s*(.+?)\s*$",
    re.I | re.M,
)
_TRAILER_VALUE = re.compile(r"^(.+?)\s*<([^>]+)>\s*$")

# Versioned / dotted model ids (same family as check_pr_body.py).
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
# AI product / model names that must not appear as author or committer.
_AI_IDENTITY_NAME = re.compile(
    r"(?i)\b(?:"
    r"Claude|Anthropic|ChatGPT|OpenAI|Gemini|Copilot|Codex|"
    r"Cursor Agent|cursoragent|Aider|Devin|Composer"
    r")\b"
)
_GENERATED_WITH = re.compile(r"Generated with", re.I)


def _run(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )


def identity_ok(name: str, email: str) -> bool:
    return name == ALLOWED_NAME and email.lower() == ALLOWED_EMAIL.lower()


def email_ok_in_text(email: str) -> bool:
    """True when an address may appear in added file lines."""
    low = email.lower()
    if low == ALLOWED_EMAIL.lower():
        return True
    return bool(_GENERIC_EMAIL.match(email))


def _parse_trailer_value(value: str) -> tuple[str, str] | None:
    match = _TRAILER_VALUE.match(value.strip())
    if not match:
        return None
    return match.group(1).strip(), match.group(2).strip()


def _model_hits(text: str) -> list[str]:
    return sorted({m.group(0) for m in _MODEL_ID.finditer(text)})


def check_commit(
    sha: str,
    author: str,
    email: str,
    committer: str,
    cemail: str,
    body: str,
) -> list[str]:
    """Return problem lines for one commit's identity, trailers, and models."""
    errors: list[str] = []
    short = sha[:7]
    if not identity_ok(author, email):
        errors.append(f"{short}: author {author} <{email}> (want {ALLOWED})")
    if not identity_ok(committer, cemail):
        errors.append(f"{short}: committer {committer} <{cemail}> (want {ALLOWED})")

    for field, value in (("author", author), ("committer", committer)):
        if _AI_IDENTITY_NAME.search(value):
            errors.append(f"{short}: {field} name has AI model/product token: {value!r}")
        for model in _model_hits(value):
            errors.append(f"{short}: {field} name has model id {model!r}")

    for model in _model_hits(body):
        errors.append(f"{short}: commit message has model id {model!r}")
    if _GENERATED_WITH.search(body):
        errors.append(f'{short}: commit message has "Generated with" attribution')

    for match in _FORBIDDEN_TRAILER.finditer(body):
        kind = match.group(1)
        errors.append(
            f"{short}: forbidden trailer {kind} "
            "(no Co-Authored-By, Reviewed-by, or Claude trailers)"
        )

    for match in _SIGNED_OFF.finditer(body):
        value = match.group(1)
        parsed = _parse_trailer_value(value)
        if parsed is None:
            errors.append(f"{short}: Signed-off-by has no Name <email>: {value!r}")
            continue
        name, addr = parsed
        if not identity_ok(name, addr):
            errors.append(f"{short}: Signed-off-by {name} <{addr}> (want {ALLOWED})")
    return errors


def check_added_text(sha: str, path: str, text: str) -> list[str]:
    """Return problems for personal-looking emails in added lines."""
    errors: list[str] = []
    short = sha[:7]
    for line in text.splitlines():
        if not line.startswith("+") or line.startswith("+++"):
            continue
        for addr in _EMAIL.findall(line):
            if not email_ok_in_text(addr):
                errors.append(
                    f"{short}: added {path} has {addr} (only {ALLOWED_EMAIL} or generic examples)"
                )
    return errors


def check_range(repo: Path | str, rev_range: str) -> list[str]:
    """Check every commit in rev_range. Return problem lines (empty when clean)."""
    root = Path(repo)
    listed = _run(root, "rev-list", rev_range)
    if listed.returncode != 0:
        detail = listed.stderr.strip() or listed.stdout.strip()
        return [f"git rev-list failed for {rev_range!r}: {detail}"]
    shas = [line.strip() for line in listed.stdout.splitlines() if line.strip()]
    errors: list[str] = []
    for sha in shas:
        meta = _run(
            root,
            "show",
            "-s",
            "--format=%an%n%ae%n%cn%n%ce%n%B",
            sha,
        )
        if meta.returncode != 0:
            errors.append(f"{sha[:7]}: git show failed")
            continue
        parts = meta.stdout.split("\n", 4)
        if len(parts) < 5:
            errors.append(f"{sha[:7]}: could not parse commit metadata")
            continue
        author, email, committer, cemail, body = parts
        errors += check_commit(sha, author, email, committer, cemail, body)
        diff = _run(root, "show", "--format=", "--unified=0", sha)
        if diff.returncode != 0:
            errors.append(f"{sha[:7]}: git show diff failed")
            continue
        path = ""
        chunk: list[str] = []
        for line in diff.stdout.splitlines():
            if line.startswith("diff --git "):
                if path and chunk and not _skip_file_email_scan(path):
                    errors += check_added_text(sha, path, "\n".join(chunk))
                chunk = []
                # diff --git a/path b/path
                bits = line.split(" b/", 1)
                path = bits[1] if len(bits) == 2 else line
            elif line.startswith("+++ "):
                continue
            else:
                chunk.append(line)
        if path and chunk and not _skip_file_email_scan(path):
            errors += check_added_text(sha, path, "\n".join(chunk))
    return errors


def _skip_file_email_scan(path: str) -> bool:
    """True when added-line email rules do not apply (test fixtures)."""
    norm = path.replace("\\", "/").lstrip("./")
    return norm == "tests" or norm.startswith("tests/")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "range",
        nargs="?",
        default="origin/main..HEAD",
        help="git revision range (default: origin/main..HEAD)",
    )
    parser.add_argument(
        "--repo",
        default=".",
        help="git repository path (default: .)",
    )
    args = parser.parse_args(argv)
    problems = check_range(Path(args.repo), args.range)
    if not problems:
        print(f"identity: ok ({args.range})")
        return 0
    print(f"identity: {len(problems)} problem(s) in {args.range}", file=sys.stderr)
    for line in problems:
        print(f"error: {line}", file=sys.stderr)
    print(
        f"Fix: use {ALLOWED} for author and committer; "
        "no Co-Authored-By, Reviewed-by, Claude trailers, or AI model names.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
