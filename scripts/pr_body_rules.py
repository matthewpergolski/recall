"""PR body rules shared by the local command and CI.

CI calls this module. It does not keep a second copy of the rules.
"""

from __future__ import annotations

import json
import re

REQUIRED_HEADINGS = [
    "TL;DR",
    "How it works",
    "Summary",
    "Files changed",
    "How to test",
    "Review",
]

# Level-2 headings only. A single '#' must not satisfy the six-section rules.
_HEADING = re.compile(r"^##\s+(.+?)\s*$", re.M)
_MERMAID = re.compile(r"^```mermaid\s*$", re.M | re.I)

# Versioned / dotted model ids. Bare names such as "Grok 4.7" are not ids.
# No bare cursor-... alternative: branch names must not match.
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
    r"codex-[\w.\-]+"
    r")\b"
)
_ASSET_SUFFIX = re.compile(
    r"\.(?:png|jpe?g|gif|webp|svg|ico|js|css|map|md|txt)$",
    re.I,
)
_GENERATED_WITH = re.compile(r"Generated with", re.I)
_OPEN_PHRASES = ("Open in Cursor", "Open in Web")
_BEGIN = re.compile(r"<!--\s*CURSOR_AGENT_PR_BODY_BEGIN\s*-->")
_END = re.compile(r"<!--\s*CURSOR_AGENT_PR_BODY_END\s*-->")


def plan_body_update(text: str) -> str | None:
    """Return the inner body when Cursor markers are present, else None.

    None means do not write. A second call on the cleaned body returns None,
    so a later `edited` event does not patch again.
    """
    begin = _BEGIN.search(text)
    if not begin:
        return None
    end = _END.search(text, begin.end())
    if not end:
        return None
    inner = text[begin.end() : end.start()]
    inner = inner.replace("\r\n", "\n").replace("\r", "\n").strip("\n")
    if not inner.strip():
        return None
    cleaned = inner + "\n"
    # A later mention of the marker name is prose. Only a leftover comment blocks the write.
    if _BEGIN.search(cleaned) or _END.search(cleaned) or cleaned == text:
        return None
    return cleaned


def should_write_body(text: str, *, actor: str = "", action: str = "") -> str | None:
    """Decide whether this event should PATCH the PR body.

    github-actions[bot] on `edited` is the loop guard: that actor is the
    GITHUB_TOKEN edit coming back, and it must not patch again.
    """
    if action == "edited" and actor == "github-actions[bot]":
        return None
    return plan_body_update(text)


def explain_github_denied(status: int, payload: str) -> str:
    """Say why GITHUB_TOKEN could not edit the body, and the smallest next step."""
    message = payload.strip()
    try:
        parsed = json.loads(payload)
    except json.JSONDecodeError:
        parsed = None
    if isinstance(parsed, dict) and parsed.get("message"):
        message = str(parsed["message"])
    return (
        "pr-body: GITHUB_TOKEN cannot edit this pull request body.\n"
        f"GitHub said: {status} {message}\n"
        "Why: a pull_request job's GITHUB_TOKEN is read-only when the pull "
        "request comes from a fork. It is also read-only when the repository "
        "Actions setting grants read and this job does not set "
        "pull-requests: write. This job sets pull-requests: write, which is "
        "enough for a branch in this repository.\n"
        "Minimal alternative: save the body to a file and run "
        "`scripts/pr_body_check <file> --head <sha> --fix`, then put that text "
        "on the PR. Do not switch this job to pull_request_target. "
        "Do not use --no-verify."
    )


def check_body(text: str, head: str | None = None) -> list[str]:
    """Return problem lines for one PR body (empty when clean)."""
    from pr_body_sections import how_to_test_errors, review_errors

    errors: list[str] = []
    errors.extend(_marker_errors(text))
    errors.extend(_heading_errors(text))
    errors.extend(review_errors(text, head))
    errors.extend(how_to_test_errors(text))
    return errors


def _marker_errors(text: str) -> list[str]:
    errors: list[str] = []
    if _BEGIN.search(text) or _END.search(text):
        errors.append(
            "CURSOR_AGENT_PR_BODY marker found. "
            "Fix: remove CURSOR_AGENT_PR_BODY HTML comment markers and the "
            "agent footer from the PR body. "
            "`scripts/pr_body_check <file> --fix` keeps only the text between the markers."
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
            "Fix: use a bare name such as Grok 4.7. "
            "A versioned id such as grok-4.7 is not allowed. "
            "Product names (Claude, Cursor, Codex) are fine."
        )
    if not _MERMAID.search(text):
        errors.append(
            "missing ```mermaid fence. Fix: add a ```mermaid code fence (under How it works)."
        )
    return errors


def _heading_errors(text: str) -> list[str]:
    found: list[str] = []
    for match in _HEADING.finditer(text):
        title = match.group(1).strip()
        if title in REQUIRED_HEADINGS:
            found.append(title)
    missing = [h for h in REQUIRED_HEADINGS if h not in found]
    if missing:
        return [
            "missing required heading(s): "
            + ", ".join(missing)
            + ". Fix: add the six headings in order: "
            + ", ".join(REQUIRED_HEADINGS)
            + "."
        ]
    want = list(REQUIRED_HEADINGS)
    pos = 0
    for title in found:
        if pos < len(want) and title == want[pos]:
            pos += 1
    if pos != len(want):
        return [
            "required headings are out of order. "
            "Fix: use the six headings in order: " + ", ".join(REQUIRED_HEADINGS) + "."
        ]
    return []
