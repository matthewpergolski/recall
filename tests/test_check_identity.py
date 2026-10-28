"""scripts/check_identity.py: only the neutral Recall identity may land."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check_identity.py"

NEUTRAL_NAME = "recall contributors"
NEUTRAL_EMAIL = "contributors@recall.invalid"


def load_mod():
    spec = importlib.util.spec_from_file_location("check_identity", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


def git(cwd: Path, *args: str, env: dict | None = None) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
        env=env,
    )
    return result.stdout


def init_repo(path: Path, *, name: str = NEUTRAL_NAME, email: str = NEUTRAL_EMAIL) -> Path:
    path.mkdir(parents=True)
    git(path, "init", "-q", "-b", "main")
    git(path, "config", "user.name", name)
    git(path, "config", "user.email", email)
    (path / "README.md").write_text("start\n")
    git(path, "add", "README.md")
    git(path, "commit", "-q", "-m", "start")
    return path


def base_env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items()}


class CheckIdentityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def test_neutral_range_is_clean(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "ok")
            (repo / "a.txt").write_text("ok\n")
            git(repo, "add", "a.txt")
            git(repo, "commit", "-q", "-m", "add a")
            self.assertEqual(self.mod.check_range(repo, "HEAD~1..HEAD"), [])

    def test_foreign_author_email_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "bad-author")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            env = {
                **base_env(),
                "GIT_AUTHOR_NAME": NEUTRAL_NAME,
                "GIT_AUTHOR_EMAIL": "stranger@example.net",
                "GIT_COMMITTER_NAME": NEUTRAL_NAME,
                "GIT_COMMITTER_EMAIL": NEUTRAL_EMAIL,
            }
            git(repo, "commit", "-q", "-m", "bad author", env=env)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(errors)
            self.assertTrue(
                any("author" in e.lower() and "stranger@example.net" in e for e in errors)
            )

    def test_foreign_committer_email_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "bad-committer")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            env = {
                **base_env(),
                "GIT_AUTHOR_NAME": NEUTRAL_NAME,
                "GIT_AUTHOR_EMAIL": NEUTRAL_EMAIL,
                "GIT_COMMITTER_NAME": NEUTRAL_NAME,
                "GIT_COMMITTER_EMAIL": "other@example.net",
            }
            git(repo, "commit", "-q", "-m", "bad committer", env=env)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(
                any("committer" in e.lower() and "other@example.net" in e for e in errors)
            )

    def test_co_authored_by_trailer_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "trailer")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            msg = "feat\n\nCo-authored-by: Someone Else <else@example.net>\n"
            git(repo, "commit", "-q", "-m", msg)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any("co-authored-by" in e.lower() for e in errors))

    def test_co_authored_by_claude_trailer_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "claude-trailer")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            msg = "feat\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n"
            git(repo, "commit", "-q", "-m", msg)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any("co-authored-by" in e.lower() for e in errors))

    def test_reviewed_by_trailer_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "reviewed")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            msg = "feat\n\nReviewed-by: Reviewer <reviewer@example.net>\n"
            git(repo, "commit", "-q", "-m", msg)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any("reviewed-by" in e.lower() for e in errors))

    def test_foreign_signed_off_by_trailer_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "sob")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            msg = "feat\n\nSigned-off-by: Someone Else <else@example.net>\n"
            git(repo, "commit", "-q", "-m", msg)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(
                any("signed-off-by" in e.lower() and "else@example.net" in e for e in errors)
            )

    def test_model_id_in_commit_message_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "model-msg")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            git(repo, "commit", "-q", "-m", "shipped with claude-opus-4 help")
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any("claude-opus-4" in e for e in errors))

    def test_ai_name_in_author_is_reported(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "ai-author")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            env = {
                **base_env(),
                "GIT_AUTHOR_NAME": "Claude Opus",
                "GIT_AUTHOR_EMAIL": NEUTRAL_EMAIL,
                "GIT_COMMITTER_NAME": NEUTRAL_NAME,
                "GIT_COMMITTER_EMAIL": NEUTRAL_EMAIL,
            }
            git(repo, "commit", "-q", "-m", "bad author name", env=env)
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any("author" in e.lower() for e in errors))

    def test_added_personal_email_in_file_is_reported(self) -> None:
        import tempfile

        # Build the address at runtime so this test file itself stays clean.
        addr = "person@" + "somewhere.org"
        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "file-email")
            (repo / "note.md").write_text(f"contact me at {addr} please\n")
            git(repo, "add", "note.md")
            git(repo, "commit", "-q", "-m", "add note")
            errors = self.mod.check_range(repo, "HEAD~1..HEAD")
            self.assertTrue(any(addr in e for e in errors))

    def test_generic_example_emails_in_files_are_allowed(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "generic")
            (repo / "note.md").write_text(
                f"neutral {NEUTRAL_EMAIL}\n"
                "clone git@github.com:org/repo.git\n"
                "tests use test@example.invalid\n"
            )
            git(repo, "add", "note.md")
            git(repo, "commit", "-q", "-m", "generic examples")
            self.assertEqual(self.mod.check_range(repo, "HEAD~1..HEAD"), [])

    def test_personal_email_under_tests_path_is_allowed(self) -> None:
        import tempfile

        addr = "person@" + "somewhere.org"
        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "tests-dir")
            (repo / "tests").mkdir()
            (repo / "tests" / "fixture.txt").write_text(f"{addr}\n")
            git(repo, "add", "tests/fixture.txt")
            git(repo, "commit", "-q", "-m", "fixture under tests")
            self.assertEqual(self.mod.check_range(repo, "HEAD~1..HEAD"), [])

    def test_cli_exits_1_on_bad_range(self) -> None:
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            repo = init_repo(Path(tmp) / "cli")
            (repo / "a.txt").write_text("x\n")
            git(repo, "add", "a.txt")
            env = {
                **base_env(),
                "GIT_AUTHOR_NAME": "Nope",
                "GIT_AUTHOR_EMAIL": "nope@example.net",
                "GIT_COMMITTER_NAME": NEUTRAL_NAME,
                "GIT_COMMITTER_EMAIL": NEUTRAL_EMAIL,
            }
            git(repo, "commit", "-q", "-m", "bad", env=env)
            code = subprocess.run(
                ["python3", str(SCRIPT), "HEAD~1..HEAD"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
            ).returncode
            self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
