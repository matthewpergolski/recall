"""A hook's git variables must not let tests commit onto this checkout."""

from __future__ import annotations

import os
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Modules that build temp git repos. This file stays out of that list so it
# does not start itself again.
GIT_SUITES = (
    "tests.test_check_identity",
    "tests.test_check_sh_gates",
    "tests.test_check_version_bump",
    "tests.test_check_workflow",
)


def _git(*args: str) -> str:
    env = os.environ.copy()
    for key in (
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ):
        env.pop(key, None)
    result = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        env=env,
    )
    return result.stdout.strip()


def _abs(path: str) -> str:
    candidate = Path(path)
    if candidate.is_absolute():
        return str(candidate)
    return str((ROOT / candidate).resolve())


class HookGitEnvTests(unittest.TestCase):
    def test_temp_repo_tests_leave_this_checkout_untouched(self) -> None:
        if os.environ.get("RECALL_HOOK_ENV_CHILD") == "1":
            return
        before_head = _git("rev-parse", "HEAD")
        before_tree = _git("rev-parse", "HEAD^{tree}")
        before_status = _git("status", "--porcelain")
        before_index = _git("diff", "--cached", "--name-only")
        before_origin = _git("rev-parse", "refs/remotes/origin/main")
        git_dir = _abs(_git("rev-parse", "--absolute-git-dir"))
        common = _abs(_git("rev-parse", "--git-common-dir"))
        env = os.environ.copy()
        env.update(
            {
                "GIT_DIR": git_dir,
                "GIT_WORK_TREE": str(ROOT),
                "GIT_INDEX_FILE": str(Path(git_dir) / "index"),
                "GIT_COMMON_DIR": common,
                "GIT_OBJECT_DIRECTORY": str(Path(common) / "objects"),
                "RECALL_HOOK_ENV_CHILD": "1",
            }
        )
        try:
            result = subprocess.run(
                ["python3", "-m", "unittest", *GIT_SUITES, "-q"],
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertEqual(_git("rev-parse", "HEAD"), before_head)
            self.assertEqual(_git("rev-parse", "HEAD^{tree}"), before_tree)
            self.assertEqual(_git("status", "--porcelain"), before_status)
            self.assertEqual(_git("diff", "--cached", "--name-only"), before_index)
            self.assertEqual(
                _git("rev-parse", "refs/remotes/origin/main"), before_origin
            )
        finally:
            if _git("rev-parse", "HEAD") != before_head:
                subprocess.run(
                    ["git", "reset", "--hard", before_head],
                    cwd=ROOT,
                    check=False,
                )
            if _git("rev-parse", "refs/remotes/origin/main") != before_origin:
                subprocess.run(
                    [
                        "git",
                        "update-ref",
                        "refs/remotes/origin/main",
                        before_origin,
                    ],
                    cwd=ROOT,
                    check=False,
                )
