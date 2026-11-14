"""Review and How to test rules for a Recall pull request body."""

from __future__ import annotations

import re

_SECTION = re.compile(r"^## ([^\n]+)\s*$", re.M)
_BARE_MODEL = re.compile(
    r"(?i)^(Grok|Claude|GPT|Gemini|Composer|Codex)(?:[ ]+[A-Za-z0-9.]+)+$"
)
_IMPL = re.compile(r"(?mi)^[ \t>*-]*Implementer:\s*(.+?)\s*$")
_REVIEWER = re.compile(r"(?mi)^[ \t>*-]*Reviewer:\s*(.+?)\s*$")
_HEAD_LINE = re.compile(r"(?mi)^[ \t>*-]*Head:\s*`?([0-9a-fA-F]{40})`?\s*$")
_TOP_BULLET = re.compile(r"^[ \t]*[-*][ \t]+([^:]+):\s*(.*)$")
_SKIP_RESULT = re.compile(
    r"(?i)^(not run|not needed|not tested|skipped|pending|todo|tbd|n/?a|none)\b"
)
_WHY_SPLIT = (" — ", " - ", " -- ")
_FAMILIES = ("grok", "claude", "gpt", "gemini", "composer", "codex")


def review_errors(text: str, head: str | None, *, draft: bool | None = None) -> list[str]:
    review = _section(text, "Review")
    if not review:
        return []
    errors: list[str] = []
    impl_match = _IMPL.search(review)
    rev_match = _REVIEWER.search(review)
    impl = _name_from(impl_match.group(1)) if impl_match else None
    reviewer = _name_from(rev_match.group(1)) if rev_match else None
    if reviewer == "pending" and draft is not True:
        errors.append(
            "Reviewer: pending is only allowed on a draft pull request. "
            "Fix: mark the pull request as a draft, or set Reviewer to the "
            "model that reviewed it."
        )
    if impl is None:
        errors.append(
            "Review section needs an Implementer line with a bare model name "
            "such as Grok 4.7. Fix: add `Implementer: <model>`."
        )
    if reviewer is None:
        errors.append(
            "Review section needs a Reviewer line with a bare model name "
            "such as Claude Opus 4.6. Fix: add `Reviewer: <model>`."
        )
    if impl and reviewer and impl.casefold() == reviewer.casefold():
        errors.extend(_same_model_errors(review, impl))
    head_match = _HEAD_LINE.search(review)
    if not head_match:
        errors.append(
            "Head: line must be a full 40-character SHA. "
            "Fix: add `Head: `<sha>`` under Review."
        )
    elif head is not None and head_match.group(1).lower() != head.lower():
        found = head_match.group(1).lower()
        errors.append(
            f"Head: {found} must equal the PR head SHA {head.lower()}. "
            "Fix: put the current pull request head SHA on the Head: line."
        )
    return errors


def how_to_test_errors(text: str) -> list[str]:
    section = _section(text, "How to test")
    if not section:
        return []
    entries = _entries(section)
    errors = _end_to_end_errors(entries.get("end-to-end"))
    errors.extend(_mac_only_errors(entries.get("mac-only")))
    return errors


def _section(text: str, title: str) -> str:
    match = re.search(rf"^## {re.escape(title)}\s*$", text, re.M)
    if not match:
        return ""
    rest = text[match.end() :]
    nxt = _SECTION.search(rest)
    if nxt:
        rest = rest[: nxt.start()]
    return rest


def _name_from(value: str) -> str | None:
    # Imported lazily so this module and pr_body_rules do not import each other at load.
    from pr_body_rules import _MODEL_ID

    cleaned = value.strip().strip("`").strip()
    if not cleaned or _MODEL_ID.search(cleaned):
        return None
    if cleaned.casefold() == "pending":
        return "pending"
    if _BARE_MODEL.match(cleaned):
        return re.sub(r"\s+", " ", cleaned)
    return None


def _same_model_errors(review: str, impl: str) -> list[str]:
    errors: list[str] = []
    low = review.casefold()
    if "fresh agent" not in low:
        errors.append(
            "Implementer and Reviewer are the same model. "
            "The Review section must say this was a fresh agent. "
            "Fix: name the other models you tried, why each was unavailable, "
            "and say the reviewer was a fresh agent, not the implementing agent."
        )
    if "not the implementing agent" not in low and "never the implementing agent" not in low:
        errors.append(
            "Implementer and Reviewer are the same model. "
            "The Review section must say the reviewer was not the implementing agent."
        )
    family = impl.split()[0].casefold()
    if not any(re.search(rf"\b{name}\b", review, re.I) for name in _FAMILIES if name != family):
        errors.append(
            "Implementer and Reviewer are the same model. "
            "Name the other models you tried."
        )
    if not re.search(
        r"unavailable|refused|quota|rate limit|could not|couldn't|not available",
        review,
        re.I,
    ):
        errors.append(
            "Implementer and Reviewer are the same model. "
            "Say why the other models were unavailable."
        )
    return errors


def _entries(section: str) -> dict[str, list[str]]:
    entries: dict[str, list[str]] = {}
    current: str | None = None
    buf: list[str] = []
    for line in section.splitlines():
        match = _TOP_BULLET.match(line)
        indented = line.startswith((" ", "\t"))
        if match and not indented:
            if current is not None:
                entries[current] = buf
            current = match.group(1).strip().casefold()
            buf = [match.group(2)]
        elif current is not None:
            buf.append(line)
    if current is not None:
        entries[current] = buf
    return {key: "\n".join(lines).strip() for key, lines in entries.items()}


def _is_command(text: str) -> bool:
    stripped = text.strip()
    if re.fullmatch(r"(?i)not run\.?", stripped):
        return False
    return len(stripped) >= 2 and bool(re.search(r"[A-Za-z]", stripped))


def _end_to_end_errors(body: str | None) -> list[str]:
    if body is None:
        return [
            "End-to-end: line is missing. "
            "Fix: add one End-to-end line with the command you ran and the result you saw."
        ]
    commands = [item for item in re.findall(r"`([^`]+)`", body) if _is_command(item)]
    result = re.sub(r"`[^`]*`", " ", body)
    result = re.sub(r"\s+", " ", result).strip(" -—")
    errors: list[str] = []
    if not commands:
        errors.append(
            "End-to-end: line has no command. "
            "Fix: put the command you ran in backticks, then the result you saw."
        )
    if not result or _SKIP_RESULT.match(result):
        errors.append(
            "End-to-end: 'Not run' alone fails. "
            "Fix: record the command you ran and the result you saw. "
            "Put a step that can run only on a Mac on the Mac-only: list "
            "and name the step and why."
        )
    return errors


def _mac_only_errors(body: str | None) -> list[str]:
    if body is None:
        return []
    steps: list[str] = []
    for line in body.splitlines():
        item = re.sub(r"^[ \t]*[-*][ \t]+", "", line).strip()
        if item:
            steps.append(item)
    if not steps:
        return [
            "Mac-only: list has no steps. "
            "Fix: name each step and why it cannot run here."
        ]
    errors: list[str] = []
    for step in steps:
        if not _has_why(step):
            errors.append(
                f"Mac-only: {step!r} must name the step and why. "
                "Fix: use `<step>` — <why it needs a Mac>."
            )
    return errors


def _has_why(step: str) -> bool:
    for sep in _WHY_SPLIT:
        if sep in step:
            left, right = step.split(sep, 1)
            left = left.strip().strip("`").strip()
            return bool(left) and len(right.split()) >= 2
    return False
