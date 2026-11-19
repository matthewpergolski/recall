"""scripts/check_pr_body.py: PR descriptions stay human-shaped and unmarked."""

from __future__ import annotations

import importlib.util
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check_pr_body.py"

HEADINGS = [
    "TL;DR",
    "How it works",
    "Summary",
    "Files changed",
    "How to test",
    "Review",
]


def load_mod():
    spec = importlib.util.spec_from_file_location("check_pr_body", SCRIPT)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def good_body(*, mention_products: bool = False) -> str:
    products = ""
    if mention_products:
        products = "\nHooks cover Claude, Cursor, and Codex as product names in adapters.\n"
    return f"""## TL;DR

After merge, CI rejects unmarked or AI-attributed PR bodies.
{products}
## How it works

```mermaid
flowchart LR
  A[PR body] --> B{{Shape and markers ok?}}
  B -->|no| C[Fail CI]
  B -->|yes| D[Pass]
```

Notice that the check reads the PR body text only.

## Summary

Adds a PR-body shape check and a template. Leaves the identity script's
commit rules alone.

## Files changed

| Purpose | Files |
| --- | --- |
| PR body check | `scripts/check_pr_body.py`, `tests/test_check_pr_body.py` |
| Template | `.github/pull_request_template.md` |

## How to test

- Unit: `python3 -m unittest tests.test_check_pr_body -q` — passed
- Smoke: `python3 scripts/pr_body_check body.md --head 0000000000000000000000000000000000000000` — printed ok
- End-to-end: `python3 -m unittest tests.test_check_pr_body -q` — printed ok and exited 0
- Mac-only:
  - `swift build` — needs macOS 14

## Review

Implementer: Grok 4.7
Reviewer: Claude Opus 4.6

- none yet

Head: `0000000000000000000000000000000000000000`
"""


class CheckPrBodyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def test_good_body_is_clean(self) -> None:
        self.assertEqual(self.mod.check_body(good_body()), [])

    def test_product_names_claude_cursor_codex_are_allowed(self) -> None:
        self.assertEqual(self.mod.check_body(good_body(mention_products=True)), [])

    def test_cursor_agent_pr_body_marker_is_reported(self) -> None:
        body = good_body() + "\n<!-- CURSOR_AGENT_PR_BODY_BEGIN -->\n"
        errors = self.mod.check_body(body)
        self.assertTrue(errors)
        self.assertTrue(any("CURSOR_AGENT_PR_BODY" in e for e in errors))
        self.assertIn("Fix:", "\n".join(errors))

    def test_open_in_cursor_or_web_footer_is_reported(self) -> None:
        for phrase in ("Open in Cursor", "Open in Web"):
            with self.subTest(phrase=phrase):
                body = good_body() + f'\n<img alt="{phrase}" src="x.png">\n'
                errors = self.mod.check_body(body)
                self.assertTrue(
                    any(phrase in e or "Open in" in e for e in errors),
                    phrase,
                )
                self.assertIn("Fix:", "\n".join(errors))

    def test_generated_with_is_reported(self) -> None:
        body = good_body().replace(
            "- none yet\n",
            "- none yet\n\nGenerated with Claude\n",
        )
        errors = self.mod.check_body(body)
        self.assertTrue(any("Generated with" in e for e in errors))
        self.assertIn("Fix:", "\n".join(errors))

    def test_model_ids_are_reported_but_bare_product_names_are_not(self) -> None:
        clean = good_body(mention_products=True)
        self.assertEqual(self.mod.check_body(clean), [])
        for model in (
            "claude-opus-4",
            "gpt-4o",
            "gemini-2.5-pro",
            "composer-1",
            "o3-mini",
        ):
            with self.subTest(model=model):
                body = clean.replace(
                    "Hooks cover Claude, Cursor, and Codex as product names in adapters.",
                    f"Ran on {model} for this change.",
                )
                errors = self.mod.check_body(body)
                self.assertTrue(any(model in e for e in errors), model)
                self.assertIn("Fix:", "\n".join(errors))

    def test_cursor_asset_filenames_are_not_model_ids(self) -> None:
        body = good_body().replace(
            "- none yet\n",
            "- none yet\n\n![x](https://example.com/open-in-cursor-dark.png)\n",
        )
        self.assertFalse(any("model id" in e for e in self.mod.check_body(body)))

    def test_branch_like_cursor_token_is_not_a_model_id(self) -> None:
        """F4: hyphenated cursor-... branch names must not trip model-id rules."""
        body = good_body().replace(
            "- none yet\n",
            "- none yet\n\nBranch: cursor-pr-template-identity-3d92\n",
        )
        errors = self.mod.check_body(body)
        self.assertEqual(errors, [])
        self.assertFalse(any("model id" in e for e in errors))

    def test_missing_heading_is_reported(self) -> None:
        body = good_body().replace("## Review\n", "## Notes\n")
        errors = self.mod.check_body(body)
        self.assertTrue(any("Review" in e for e in errors))
        self.assertIn("Fix:", "\n".join(errors))

    def test_headings_out_of_order_are_reported(self) -> None:
        body = good_body()
        body = body.replace("## Summary\n", "## FILES_TMP\n", 1)
        body = body.replace("## Files changed\n", "## Summary\n", 1)
        body = body.replace("## FILES_TMP\n", "## Files changed\n", 1)
        errors = self.mod.check_body(body)
        self.assertTrue(any("order" in e.lower() or "heading" in e.lower() for e in errors))
        self.assertIn("Fix:", "\n".join(errors))

    def test_missing_mermaid_fence_is_reported(self) -> None:
        body = good_body().replace("```mermaid\n", "```\n", 1)
        errors = self.mod.check_body(body)
        self.assertTrue(any("mermaid" in e.lower() for e in errors))
        self.assertIn("Fix:", "\n".join(errors))

    def test_cli_exits_1_on_bad_body(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "body.md"
            bad.write_text("no headings\n", encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), str(bad)],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("Fix:", result.stderr)

    def test_cli_exits_0_on_good_body(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "body.md"
            path.write_text(good_body(), encoding="utf-8")
            result = subprocess.run(
                ["python3", str(SCRIPT), str(path)],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertIn("pr-body: ok", result.stdout)

    def test_required_headings_constant_matches_template_expectation(self) -> None:
        self.assertEqual(list(self.mod.REQUIRED_HEADINGS), HEADINGS)

    def test_pending_reviewer_passes_on_a_draft(self) -> None:
        errors = self.mod.check_body(
            filled_body(reviewer="pending"), head=HEAD_SHA, draft=True
        )
        self.assertEqual(errors, [])

    def test_pending_reviewer_fails_when_the_pull_request_is_ready(self) -> None:
        errors = self.mod.check_body(
            filled_body(reviewer="pending"), head=HEAD_SHA, draft=False
        )
        self.assertTrue(any("draft" in error for error in errors), errors)
        self.assertTrue(any("Fix:" in error for error in errors), errors)

    def test_pull_request_template_passes_the_body_check(self) -> None:
        template = ROOT / ".github" / "pull_request_template.md"
        self.assertTrue(
            template.is_file(),
            "add .github/pull_request_template.md with the six headings",
        )
        errors = self.mod.check_body(template.read_text(encoding="utf-8"))
        self.assertEqual(errors, [])


HEAD_SHA = "0123456789abcdef0123456789abcdef01234567"
OTHER_SHA = "fedcba9876543210fedcba9876543210fedcba98"
_BEGIN = "<!-- CURSOR_AGENT_PR_BODY_BEGIN -->"
_END = "<!-- CURSOR_AGENT_PR_BODY_END -->"


def filled_body(*, implementer: str = "Grok 4.7", reviewer: str = "Claude Opus 4.6") -> str:
    """Body that satisfies the six-section rules, including the new lines."""
    return f"""## TL;DR

After merge, local hooks and CI reject a PR that breaks the body rules.

## How it works

```mermaid
flowchart LR
  A[Commit] --> B[Hook]
  B --> C[CI]
```

The diagram shows the hook and then CI.

## Summary

The checks live in scripts. The hooks call those scripts.

## Files changed

| Purpose | Files |
| --- | --- |
| Body rules | `scripts/check_pr_body.py` |

## How to test

- Unit: `python3 -m unittest tests.test_check_pr_body -q` — 20 passed
- Smoke: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok`
- End-to-end: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok` and exited 0
- Mac-only:
  - `swift run --package-path capture-helper recall-capture list-sources` — needs macOS 14 and a microphone permission

## Review

Implementer: {implementer}
Reviewer: {reviewer}

- none yet

Head: `{HEAD_SHA}`
"""


class CheckPrBodyRuleTests(unittest.TestCase):
    """Each new body rule fails on a bad example."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def test_end_to_end_not_run_alone_fails(self) -> None:
        body = filled_body().replace(
            f"- End-to-end: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok` and exited 0",
            "- End-to-end: Not run",
        )
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("End-to-end" in e and "Not run" in e for e in errors), errors)
        self.assertIn("Fix:", "\n".join(errors))

    def test_end_to_end_command_with_not_run_result_fails(self) -> None:
        body = filled_body().replace(
            f"- End-to-end: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok` and exited 0",
            "- End-to-end: `cargo test` — Not run",
        )
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("End-to-end" in e for e in errors), errors)

    def test_section_rules_require_level2_headings(self) -> None:
        body = filled_body().replace("## ", "# ")
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(errors)
        self.assertTrue(any("heading" in e.lower() for e in errors), errors)

    def test_plan_body_update_strips_when_inner_mentions_marker_name(self) -> None:
        inner = filled_body().replace(
            "The checks live in scripts. The hooks call those scripts.",
            "The checks name the CURSOR_AGENT_PR_BODY marker in prose.",
        )
        wrapped = _BEGIN + "\n" + inner + _END + "\nOpen in Web\n"
        cleaned = self.mod.plan_body_update(wrapped)
        self.assertIsNotNone(cleaned)
        assert cleaned is not None
        self.assertIn("CURSOR_AGENT_PR_BODY", cleaned)
        self.assertNotIn("CURSOR_AGENT_PR_BODY_BEGIN", cleaned)
        self.assertNotIn("Open in Web", cleaned)
        self.assertEqual(self.mod.check_body(cleaned, head=HEAD_SHA), [])
        self.assertIsNone(self.mod.plan_body_update(cleaned))

    def test_body_without_mac_only_steps_passes(self) -> None:
        body = filled_body().replace(
            "- Mac-only:\n"
            "  - `swift run --package-path capture-helper recall-capture list-sources`"
            " — needs macOS 14 and a microphone permission\n",
            "",
        )
        self.assertNotIn("Mac-only", body)
        self.assertEqual(self.mod.check_body(body, head=HEAD_SHA), [])

    def test_end_to_end_passed_is_a_result(self) -> None:
        body = filled_body().replace(
            f"- End-to-end: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok` and exited 0",
            "- End-to-end: `cargo test` — passed",
        )
        self.assertEqual(self.mod.check_body(body, head=HEAD_SHA), [])

    def test_end_to_end_without_a_command_fails(self) -> None:
        body = filled_body().replace(
            f"- End-to-end: `python3 scripts/pr_body_check body.md --head {HEAD_SHA}` — printed `pr-body: ok` and exited 0",
            "- End-to-end: see above",
        )
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("End-to-end" in e and "command" in e.lower() for e in errors), errors)

    def test_review_without_model_names_fails(self) -> None:
        body = filled_body().replace("Implementer: Grok 4.7\nReviewer: Claude Opus 4.6\n", "")
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("Implementer" in e for e in errors), errors)
        self.assertTrue(any("Reviewer" in e for e in errors), errors)

    def test_versioned_model_id_is_rejected(self) -> None:
        body = filled_body(implementer="grok-4.7")
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("grok-4.7" in e for e in errors), errors)

    def test_same_model_without_fresh_agent_fails(self) -> None:
        body = filled_body(implementer="Grok 4.7", reviewer="Grok 4.7")
        errors = self.mod.check_body(body, head=HEAD_SHA)
        joined = "\n".join(errors).lower()
        self.assertIn("fresh agent", joined)
        self.assertTrue("unavailable" in joined or "same" in joined)

    def test_same_model_fresh_agent_passes(self) -> None:
        extra = (
            "Claude, GPT 5.6, and Gemini 4 were unavailable because the quota was refused. "
            "The reviewer was a fresh agent, not the implementing agent.\n"
        )
        body = filled_body(implementer="Grok 4.7", reviewer="grok 4.7").replace(
            "- none yet\n",
            extra + "- none yet\n",
        )
        self.assertEqual(self.mod.check_body(body, head=HEAD_SHA), [])

    def test_different_models_pass_without_the_fresh_agent_sentence(self) -> None:
        self.assertEqual(self.mod.check_body(filled_body(), head=HEAD_SHA), [])

    def test_head_sha_must_equal_the_pr_head(self) -> None:
        errors = self.mod.check_body(filled_body(), head=OTHER_SHA)
        self.assertTrue(any("Head" in e and OTHER_SHA in e for e in errors), errors)

    def test_short_head_sha_fails(self) -> None:
        body = filled_body().replace(HEAD_SHA, HEAD_SHA[:7])
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("Head" in e for e in errors), errors)

    def test_mac_only_step_without_a_why_fails(self) -> None:
        body = filled_body().replace(
            "  - `swift run --package-path capture-helper recall-capture list-sources` — needs macOS 14 and a microphone permission",
            "  - `swift test`",
        )
        errors = self.mod.check_body(body, head=HEAD_SHA)
        self.assertTrue(any("Mac-only" in e for e in errors), errors)

    def test_markers_keep_only_the_inner_text(self) -> None:
        self.assertTrue(
            hasattr(self.mod, "plan_body_update"),
            "plan_body_update must live in the PR body script",
        )
        inner = filled_body()
        wrapped = (
            "stale body above\nOpen in Cursor\n"
            + _BEGIN
            + "\n"
            + inner
            + _END
            + '\n<div><a href="https://cursor.com">Open in Web</a></div>\n'
        )
        cleaned = self.mod.plan_body_update(wrapped)
        self.assertEqual(cleaned, inner if inner.endswith("\n") else inner + "\n")
        self.assertNotIn("CURSOR_AGENT_PR_BODY", cleaned)
        self.assertNotIn("Open in Web", cleaned)
        self.assertNotIn("stale body", cleaned)
        self.assertIsNone(self.mod.plan_body_update(cleaned))

    def test_bot_edit_does_not_patch_again(self) -> None:
        self.assertTrue(hasattr(self.mod, "should_write_body"))
        wrapped = _BEGIN + "\n" + filled_body() + _END + "\n"
        self.assertIsNone(
            self.mod.should_write_body(
                wrapped,
                actor="github-actions[bot]",
                action="edited",
            )
        )
        self.assertIsNotNone(
            self.mod.should_write_body(wrapped, actor="matthewpergolski", action="opened")
        )

    def test_fix_is_idempotent(self) -> None:
        import tempfile

        inner = filled_body()
        wrapped = "old\n" + _BEGIN + "\n" + inner + "\n" + _END + "\nOpen in Cursor\n"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "body.md"
            path.write_text(wrapped, encoding="utf-8")
            first = subprocess.run(
                ["python3", str(SCRIPT), str(path), "--fix", "--head", HEAD_SHA],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            once = path.read_text(encoding="utf-8")
            self.assertNotIn("CURSOR_AGENT_PR_BODY", once)
            second = subprocess.run(
                ["python3", str(SCRIPT), str(path), "--fix", "--head", HEAD_SHA],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(path.read_text(encoding="utf-8"), once)


if __name__ == "__main__":
    unittest.main()
