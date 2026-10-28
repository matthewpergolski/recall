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

- Unit: `python3 -m unittest tests.test_check_pr_body -q` — pass
- Smoke: not needed, because the unit cases cover the CLI exits
- End-to-end: not needed, because CI only invokes the script on the event body

## Review

- none yet
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

    def test_pull_request_template_passes_the_body_check(self) -> None:
        template = ROOT / ".github" / "pull_request_template.md"
        self.assertTrue(
            template.is_file(),
            "add .github/pull_request_template.md with the six headings",
        )
        errors = self.mod.check_body(template.read_text(encoding="utf-8"))
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
