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


def _git_env() -> dict[str, str]:
    env = os.environ.copy()
    for key in (
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
    ):
        env.pop(key, None)
    return env


def _git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        env=_git_env(),
    )
    return result.stdout.strip()


def _ref(name: str) -> str | None:
    """Return a ref's SHA, or None when this checkout does not have it."""
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", name],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        env=_git_env(),
    )
    if result.returncode != 0:
        return None
    value = result.stdout.strip()
    return value or None


def _restore_ref(name: str, before: str | None) -> None:
    if _ref(name) == before:
        return
    if before is None:
        subprocess.run(
            ["git", "update-ref", "-d", name],
            cwd=ROOT,
            check=False,
            env=_git_env(),
        )
        return
    subprocess.run(
        ["git", "update-ref", name, before],
        cwd=ROOT,
        check=False,
        env=_git_env(),
    )


def _status() -> str:
    # Child imports write bytecode. That is not a commit onto this checkout.
    lines = [
        line
        for line in _git("status", "--porcelain").splitlines()
        if "__pycache__/" not in line
    ]
    return "\n".join(lines)


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
        before_status = _status()
        before_index = _git("diff", "--cached", "--name-only")
        origin = "refs/remotes/origin/main"
        before_origin = _ref(origin)
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
            self.assertEqual(_status(), before_status)
            self.assertEqual(_git("diff", "--cached", "--name-only"), before_index)
            self.assertEqual(_ref(origin), before_origin)
        finally:
            if _git("rev-parse", "HEAD") != before_head:
                subprocess.run(
                    ["git", "reset", "--hard", before_head],
                    cwd=ROOT,
                    check=False,
                    env=_git_env(),
                )
            _restore_ref(origin, before_origin)
