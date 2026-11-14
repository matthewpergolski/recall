"""scripts/check.sh must run identity and optional PR-body checks (F1 / N2).

These tests exercise check.sh behavior with stubbed cargo and early gates in a
temp git repo. They fail if check.sh skips the identity range check or ignores
PR_BODY_FILE / PR_BODY.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

from tests.git_env import without_hook_git

ROOT = Path(__file__).resolve().parents[1]
CHECK_SH = ROOT / "scripts" / "check.sh"
CHECK_IDENTITY = ROOT / "scripts" / "check_identity.py"
CHECK_PR_BODY = ROOT / "scripts" / "check_pr_body.py"
CHECK_PR_RULES = ROOT / "scripts" / "pr_body_rules.py"
CHECK_PR_SECTIONS = ROOT / "scripts" / "pr_body_sections.py"
CHECK_VERSION = ROOT / "scripts" / "check_version_bump.py"
INSTALL_HOOKS = ROOT / "scripts" / "install-git-hooks.sh"

NEUTRAL_NAME = "recall contributors"
NEUTRAL_EMAIL = "contributors@recall.invalid"

GOOD_PR_BODY = textwrap.dedent(
    """\
    ## TL;DR

    Gates run for real.

    ## How it works

    ```mermaid
    flowchart LR
      A[check.sh] --> B[identity]
    ```

    Notice the gates run after the early stubs.

    ## Summary

    Fixture body for gate tests.

    ## Files changed

    | Purpose | Files |
    | --- | --- |
    | Fixture | `body.md` |

    ## How to test

    - Unit: `python3 -m unittest tests.test_check_noop -q` — passed
    - Smoke: `python3 scripts/check_pr_body.py body.md --head {head}` — printed ok
    - End-to-end: `python3 scripts/check_pr_body.py body.md --head {head}` — printed ok and exited 0

    ## Review

    Implementer: Grok 4.7
    Reviewer: Claude Opus 4.6

    - none

    Head: `{head}`
    """
)


def git(cwd: Path, *args: str, env: dict | None = None) -> None:
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
        env=without_hook_git(env),
    )


def write_exec(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)


def good_body_file(dir_path: Path, head: str) -> Path:
    path = dir_path / "good-body.md"
    path.write_text(GOOD_PR_BODY.format(head=head), encoding="utf-8")
    return path


class CheckShGatesTests(unittest.TestCase):
    def _build_fixture(self, work: Path) -> Path:
        """Temp checkout with real check.sh + identity/pr-body, stubbed early steps."""
        scripts = work / "scripts"
        tests = work / "tests"
        scripts.mkdir(parents=True)
        tests.mkdir()
        shutil.copy(CHECK_SH, scripts / "check.sh")
        shutil.copy(CHECK_IDENTITY, scripts / "check_identity.py")
        shutil.copy(CHECK_PR_BODY, scripts / "check_pr_body.py")
        shutil.copy(CHECK_PR_RULES, scripts / "pr_body_rules.py")
        shutil.copy(CHECK_PR_SECTIONS, scripts / "pr_body_sections.py")
        shutil.copy(CHECK_VERSION, scripts / "check_version_bump.py")
        shutil.copy(INSTALL_HOOKS, scripts / "install-git-hooks.sh")
        (scripts / "check.sh").chmod(0o755)

        # check.sh runs unittest discover on test_check_*.py; one trivial
        # passing module keeps that step green without pulling in this suite.
        (tests / "test_check_noop.py").write_text(
            "import unittest\n\n"
            "class Noop(unittest.TestCase):\n"
            "    def test_ok(self):\n"
            "        self.assertTrue(True)\n",
            encoding="utf-8",
        )

        # Early gates: succeed instantly so we reach identity / PR-body.
        for name in (
            "optional_cairnkit_check.sh",
            "check_version_freshness.sh",
            "test_commit_msg_hook.sh",
            "test_cairnkit_check_gate.sh",
        ):
            write_exec(scripts / name, "#!/usr/bin/env bash\nexit 0\n")

        bin_dir = work / "bin"
        write_exec(
            bin_dir / "cargo",
            "#!/usr/bin/env bash\n# stub: skip real rust work in gate tests\nexit 0\n",
        )
        return bin_dir

    def _init_repo_with_range(self, work: Path, *, bad_tip_author: bool) -> None:
        git(work, "init", "-q", "-b", "main")
        git(work, "config", "user.name", NEUTRAL_NAME)
        git(work, "config", "user.email", NEUTRAL_EMAIL)
        (work / "README.md").write_text("base\n", encoding="utf-8")
        (work / "Cargo.toml").write_text(
            '[package]\nname = "recall"\nversion = "0.0.1"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (work / "Cargo.lock").write_text(
            '[[package]]\nname = "recall"\nversion = "0.0.1"\n',
            encoding="utf-8",
        )
        git(work, "add", "README.md", "Cargo.toml", "Cargo.lock", "scripts", "tests")
        git(work, "commit", "-q", "-m", "base")
        # origin/main is the merge base for the identity range.
        git(work, "update-ref", "refs/remotes/origin/main", "HEAD")

        (work / "tip.txt").write_text("tip\n", encoding="utf-8")
        git(work, "add", "tip.txt")
        env = {**os.environ}
        if bad_tip_author:
            env.update(
                {
                    "GIT_AUTHOR_NAME": "Nope",
                    "GIT_AUTHOR_EMAIL": "nope@example.net",
                    "GIT_COMMITTER_NAME": NEUTRAL_NAME,
                    "GIT_COMMITTER_EMAIL": NEUTRAL_EMAIL,
                }
            )
        git(work, "commit", "-q", "-m", "tip", env=env)

    def _run_check_sh(
        self,
        work: Path,
        bin_dir: Path,
        *,
        env_extra: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = {
            **os.environ,
            "PATH": f"{bin_dir}:{os.environ.get('PATH', '')}",
        }
        # Drop any inherited PR body so only the test controls it.
        env.pop("PR_BODY", None)
        env.pop("PR_BODY_FILE", None)
        if env_extra:
            env.update(env_extra)
        return subprocess.run(
            ["bash", str(work / "scripts" / "check.sh")],
            cwd=work,
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def test_check_sh_runs_identity_on_origin_main_range(self) -> None:
        """N2/F1: check.sh fails when origin/main..HEAD has a foreign author."""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp) / "repo"
            work.mkdir()
            bin_dir = self._build_fixture(work)
            self._init_repo_with_range(work, bad_tip_author=True)
            result = self._run_check_sh(work, bin_dir)
            self.assertNotEqual(
                result.returncode,
                0,
                "check.sh must fail when identity finds a foreign author on "
                f"origin/main..HEAD; stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            joined = result.stdout + result.stderr
            self.assertIn("check_identity.py", joined)
            self.assertTrue(
                "author" in joined.lower() or "identity:" in joined.lower(),
                joined,
            )

    def test_check_sh_runs_pr_body_when_body_file_given(self) -> None:
        """N2/F1: check.sh fails when PR_BODY_FILE points at a malformed body."""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp) / "repo"
            work.mkdir()
            bin_dir = self._build_fixture(work)
            self._init_repo_with_range(work, bad_tip_author=False)
            bad = work / "bad-body.md"
            bad.write_text("no headings\n", encoding="utf-8")
            result = self._run_check_sh(
                work,
                bin_dir,
                env_extra={"PR_BODY_FILE": str(bad)},
            )
            self.assertNotEqual(
                result.returncode,
                0,
                "check.sh must fail when PR_BODY_FILE is malformed; "
                f"stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            joined = result.stdout + result.stderr
            self.assertIn("check_pr_body.py", joined)
            self.assertTrue(
                "pr-body:" in joined.lower() or "heading" in joined.lower(),
                joined,
            )

    def test_check_sh_runs_pr_body_when_pr_body_env_given(self) -> None:
        """N2/F1: check.sh fails when PR_BODY env holds a malformed body."""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp) / "repo"
            work.mkdir()
            bin_dir = self._build_fixture(work)
            self._init_repo_with_range(work, bad_tip_author=False)
            result = self._run_check_sh(
                work,
                bin_dir,
                env_extra={"PR_BODY": "no headings\n"},
            )
            self.assertNotEqual(
                result.returncode,
                0,
                "check.sh must fail when PR_BODY env is malformed; "
                f"stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            joined = result.stdout + result.stderr
            self.assertIn("check_pr_body.py", joined)

    def test_check_sh_passes_identity_and_good_pr_body_file(self) -> None:
        """Control: clean tip + good PR_BODY_FILE lets the stubbed check.sh pass."""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp) / "repo"
            work.mkdir()
            bin_dir = self._build_fixture(work)
            self._init_repo_with_range(work, bad_tip_author=False)
            head = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=work,
                check=True,
                capture_output=True,
                text=True,
                env=without_hook_git(),
            ).stdout.strip()
            good = good_body_file(work, head)
            result = self._run_check_sh(
                work,
                bin_dir,
                env_extra={"PR_BODY_FILE": str(good)},
            )
            self.assertEqual(
                result.returncode,
                0,
                f"stdout={result.stdout!r} stderr={result.stderr!r}",
            )
            self.assertIn("All checks passed.", result.stdout)


if __name__ == "__main__":
    unittest.main()
