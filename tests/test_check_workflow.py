"""CI and git hooks call the local scripts. They do not reimplement the rules."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class CheckWorkflowTests(unittest.TestCase):
    def test_pr_workflow_strips_with_write_and_calls_the_scripts(self) -> None:
        text = (ROOT / ".github/workflows/pr-checks.yml").read_text(encoding="utf-8")
        self.assertIn("opened", text)
        self.assertIn("edited", text)
        self.assertIn("synchronize", text)
        self.assertIn("pull-requests: write", text)
        self.assertIn("scripts/check_pr_body.py", text)
        self.assertIn("--github", text)
        self.assertIn("scripts/check_version_bump.py", text)
        self.assertIn("scripts/check_identity.py", text)
        self.assertNotIn("CURSOR_AGENT_PR_BODY", text)

    def test_install_hooks_is_idempotent_and_does_not_replace_another_path(self) -> None:
        import os
        import shutil
        import subprocess
        import tempfile

        script = ROOT / "scripts" / "install-git-hooks.sh"
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            (repo / "scripts").mkdir(parents=True)
            shutil.copy(script, repo / "scripts" / "install-git-hooks.sh")
            subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
            first = subprocess.run(
                ["bash", "scripts/install-git-hooks.sh"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            second = subprocess.run(
                ["bash", "scripts/install-git-hooks.sh"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(second.returncode, 0, second.stderr)
            got = subprocess.run(
                ["git", "config", "--get", "core.hooksPath"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            self.assertEqual(got, ".githooks")
            subprocess.run(
                ["git", "config", "core.hooksPath", "custom-hooks"],
                cwd=repo,
                check=True,
            )
            blocked = subprocess.run(
                ["bash", "scripts/install-git-hooks.sh"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ},
            )
            self.assertNotEqual(blocked.returncode, 0, blocked.stdout)
            still = subprocess.run(
                ["git", "config", "--get", "core.hooksPath"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            self.assertEqual(still, "custom-hooks")

    def test_hooks_call_the_shared_scripts(self) -> None:
        pre_commit = ROOT / ".githooks/pre-commit"
        self.assertTrue(pre_commit.is_file(), "add .githooks/pre-commit")
        self.assertIn("check_version_bump.py", pre_commit.read_text(encoding="utf-8"))
        commit_msg = (ROOT / ".githooks/commit-msg").read_text(encoding="utf-8")
        self.assertIn("check_identity.py", commit_msg)
        pre_push = (ROOT / ".githooks/pre-push").read_text(encoding="utf-8")
        self.assertIn("check.sh", pre_push)
        self.assertIn("check_version_bump.py", pre_push)
        self.assertIn("origin/main", pre_push)
        check = (ROOT / "scripts/check.sh").read_text(encoding="utf-8")
        self.assertIn("check_version_bump.py", check)
        self.assertIn("install-git-hooks.sh", check)
        installer = ROOT / "scripts/install-git-hooks.sh"
        self.assertTrue(installer.is_file(), "add scripts/install-git-hooks.sh")
        self.assertIn("core.hooksPath", installer.read_text(encoding="utf-8"))
        wrapper = ROOT / "scripts/pr_body_check"
        self.assertTrue(wrapper.is_file(), "add scripts/pr_body_check")
        self.assertIn("check_pr_body.py", wrapper.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
