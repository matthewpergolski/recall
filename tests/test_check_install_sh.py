"""Tests for scripts/install.sh --plan and --rewrite-config."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTALL = ROOT / "scripts" / "install.sh"


def run_install(*args: str, env: dict[str, str] | None = None, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    merged = os.environ.copy()
    if env:
        merged.update(env)
    return subprocess.run(
        ["bash", str(INSTALL), *args],
        cwd=str(cwd or ROOT),
        env=merged,
        text=True,
        capture_output=True,
        check=False,
    )


def init_checkout(path: Path, *, remote: str = "https://github.com/matthewpergolski/recall.git") -> None:
    path.mkdir(parents=True, exist_ok=True)
    (path / "src").mkdir(exist_ok=True)
    (path / "capture-helper").mkdir(exist_ok=True)
    (path / "Cargo.toml").write_text(
        '[package]\nname = "recall"\nversion = "0.0.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    (path / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (path / "capture-helper" / "Package.swift").write_text("// fixture\n", encoding="utf-8")
    subprocess.run(["git", "init", "-b", "main"], cwd=path, check=True, capture_output=True)
    subprocess.run(["git", "remote", "add", "origin", remote], cwd=path, check=True, capture_output=True)
    subprocess.run(["git", "add", "."], cwd=path, check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Recall Test",
            "-c",
            "user.email=recall@example.invalid",
            "commit",
            "-m",
            "root",
        ],
        cwd=path,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "update-ref", "refs/remotes/origin/main", "HEAD"],
        cwd=path,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "branch", "--set-upstream-to=origin/main", "main"],
        cwd=path,
        check=True,
        capture_output=True,
    )


class InstallShTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="recall-install-sh-"))
        self.home = self.tmp / "home"
        self.home.mkdir()
        self.config_dir = self.home / ".config" / "recall"
        self.config_dir.mkdir(parents=True)
        self.env = {
            "HOME": str(self.home),
            "XDG_CONFIG_HOME": str(self.home / ".config"),
            "RECALL_INSTALL_SKIP_TOOLS": "1",
        }

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_plan_second_run_says_nothing_else_to_change_when_current(self) -> None:
        checkout = self.home / "Projects" / "recall"
        init_checkout(checkout)
        ignore = checkout / "capture-helper" / ".gitignore"
        ignore.write_text(".build/\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(checkout), "add", "capture-helper/.gitignore"], check=True, capture_output=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(checkout),
                "-c",
                "user.name=Recall Test",
                "-c",
                "user.email=recall@example.invalid",
                "commit",
                "-m",
                "ignore helper build",
            ],
            check=True,
            capture_output=True,
        )
        # Keep origin/main current with HEAD after the ignore commit.
        subprocess.run(
            ["git", "-C", str(checkout), "update-ref", "refs/remotes/origin/main", "HEAD"],
            check=True,
            capture_output=True,
        )
        helper = (
            checkout
            / "capture-helper"
            / ".build"
            / "debug"
            / "recall-capture"
        )
        helper.parent.mkdir(parents=True)
        helper.write_text("#!/bin/sh\necho stub\n", encoding="utf-8")
        helper.chmod(0o755)
        first = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
        self.assertIn("plan:", first.stdout)
        second = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        self.assertIn("nothing else to change", second.stdout)
        self.assertNotIn("swift build", second.stdout)
        self.assertNotIn("cargo install", second.stdout)

    def test_plan_dirty_names_file_without_reset(self) -> None:
        checkout = self.home / "Projects" / "recall"
        init_checkout(checkout)
        dirty = checkout / "dirty.txt"
        dirty.write_text("x\n", encoding="utf-8")
        result = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIn("dirty", result.stdout.lower())
        self.assertIn("dirty.txt", result.stdout)
        self.assertNotIn("reset", result.stdout.lower())
        self.assertTrue(dirty.is_file())

    def test_plan_unrelated_names_aside_and_clone(self) -> None:
        old = self.home / "Scripts" / "recall"
        init_checkout(old)
        # Orphan history so merge-base fails (true unrelated), then force-track it.
        other = self.tmp / "other"
        init_checkout(other)
        subprocess.run(
            ["git", "-C", str(other), "checkout", "--orphan", "fresh"],
            check=True,
            capture_output=True,
        )
        subprocess.run(["git", "-C", str(other), "rm", "-rf", "."], check=True, capture_output=True)
        (other / "src").mkdir(exist_ok=True)
        (other / "capture-helper").mkdir(exist_ok=True)
        (other / "Cargo.toml").write_text(
            '[package]\nname = "recall"\nversion = "0.0.1"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (other / "src" / "main.rs").write_text(
            "fn main() { /* fresh */ }\n", encoding="utf-8"
        )
        (other / "capture-helper" / "Package.swift").write_text("// fresh\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(other), "add", "."], check=True, capture_output=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(other),
                "-c",
                "user.name=Recall Test",
                "-c",
                "user.email=recall@example.invalid",
                "commit",
                "-m",
                "fresh unrelated root",
            ],
            check=True,
            capture_output=True,
        )
        other_tip = subprocess.check_output(
            ["git", "-C", str(other), "rev-parse", "HEAD"], text=True
        ).strip()
        subprocess.run(
            ["git", "-C", str(old), "remote", "add", "other", str(other)],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(old),
                "fetch",
                "--quiet",
                "other",
                f"+{other_tip}:refs/remotes/origin/main",
            ],
            check=True,
            capture_output=True,
        )
        result = run_install("--plan", "--repo", str(old), env=self.env)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIn("aside-and-clone", result.stdout)

    def test_plan_missing_canonical_mentions_clone(self) -> None:
        # Empty home: no checkout. Plan should mention clone into Projects/recall
        # without running git against the missing path (no empty-branch refuse).
        result = run_install("--plan", env=self.env, cwd=self.home)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIn("clone", result.stdout.lower())
        self.assertIn("Projects/recall", result.stdout)
        self.assertNotIn("refuse branch", result.stdout)
        self.assertNotIn("No such file or directory", result.stderr)
        self.assertIn("cargo install", result.stdout)

    def test_rewrite_config_preserves_section_and_storage(self) -> None:
        checkout = self.home / "Scripts" / "recall"
        init_checkout(checkout)
        cfg = self.config_dir / "config.toml"
        cfg.write_text(
            'storage_dir = "~/Documents/Recall/sessions"\n'
            "keep_audio = false\n"
            "\n"
            "[transcription]\n"
            'engine = "apple"\n',
            encoding="utf-8",
        )
        places = self.config_dir / "session-places.txt"
        places.write_text("/tmp/sessions-a\n", encoding="utf-8")
        places_before = places.read_bytes()

        first = run_install("--rewrite-config", str(checkout), env=self.env)
        self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
        text = cfg.read_text(encoding="utf-8")
        self.assertIn("source_dir = ", text)
        self.assertTrue(text.index("source_dir") < text.index("[transcription]"))
        self.assertIn('storage_dir = "~/Documents/Recall/sessions"', text)
        self.assertIn('engine = "apple"', text)
        self.assertEqual(places.read_bytes(), places_before)

        second = run_install("--rewrite-config", str(checkout), env=self.env)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        text2 = cfg.read_text(encoding="utf-8")
        self.assertEqual(text2.count("source_dir = "), 1)


    def test_non_tty_unrelated_refuses_without_fake_prompt(self) -> None:
        """H1: closed stdin must not print [Y/n], then refuse clearly once."""
        checkout = self.home / "Scripts" / "recall"
        init_checkout(checkout)
        other = self.tmp / "other"
        init_checkout(other)
        subprocess.run(
            ["git", "-C", str(other), "checkout", "--orphan", "fresh"],
            check=True,
            capture_output=True,
        )
        subprocess.run(["git", "-C", str(other), "rm", "-rf", "."], check=True, capture_output=True)
        (other / "src").mkdir(exist_ok=True)
        (other / "capture-helper").mkdir(exist_ok=True)
        (other / "Cargo.toml").write_text(
            '[package]\nname = "recall"\nversion = "0.0.1"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (other / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (other / "capture-helper" / "Package.swift").write_text("// fresh\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(other), "add", "."], check=True, capture_output=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(other),
                "-c",
                "user.name=Recall Test",
                "-c",
                "user.email=recall@example.invalid",
                "commit",
                "-m",
                "fresh unrelated root",
            ],
            check=True,
            capture_output=True,
        )
        other_tip = subprocess.check_output(
            ["git", "-C", str(other), "rev-parse", "HEAD"], text=True
        ).strip()
        subprocess.run(
            ["git", "-C", str(checkout), "remote", "add", "other", str(other)],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(checkout),
                "fetch",
                "--quiet",
                "other",
                f"+{other_tip}:refs/remotes/origin/main",
            ],
            check=True,
            capture_output=True,
        )
        # Marker committed so the tree stays clean for the history prompt.
        marker = checkout / "KEEP"
        marker.write_text("keep\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(checkout), "add", "KEEP"], check=True, capture_output=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(checkout),
                "-c",
                "user.name=Recall Test",
                "-c",
                "user.email=recall@example.invalid",
                "commit",
                "-m",
                "keep marker",
            ],
            check=True,
            capture_output=True,
        )
        env = dict(self.env)
        env["RECALL_INSTALL_SKIP_FETCH"] = "1"
        result = subprocess.run(
            ["bash", str(INSTALL), "--repo", str(checkout)],
            cwd=str(self.home),
            env=env,
            text=True,
            capture_output=True,
            stdin=subprocess.DEVNULL,
            check=False,
        )
        combined = result.stdout + result.stderr
        self.assertNotEqual(result.returncode, 0, combined)
        self.assertNotIn("[Y/n]", combined, combined)
        self.assertIn("--yes", result.stderr, result.stderr)
        self.assertTrue(marker.is_file())
        self.assertNotIn("Left ", result.stderr)

    def test_ssh_remote_is_accepted_as_checkout(self) -> None:
        """H2: git@github.com:owner/repo.git must pass the official remote check."""
        checkout = self.home / "Scripts" / "recall"
        init_checkout(checkout, remote="git@github.com:matthewpergolski/recall.git")
        result = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertNotIn("not a Recall checkout", result.stderr)
        self.assertIn("plan:", result.stdout)

    def test_archive_substring_remote_is_rejected(self) -> None:
        checkout = self.home / "Scripts" / "recall"
        init_checkout(
            checkout, remote="https://github.com/matthewpergolski/recall-archive.git"
        )
        result = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("not a Recall checkout", result.stderr)

    def test_invalid_source_dir_remote_stops_without_cloning(self) -> None:
        pinned = self.home / "Scripts" / "recall"
        init_checkout(pinned, remote="https://github.com/other/not-recall.git")
        cfg = self.config_dir / "config.toml"
        cfg.write_text(f'source_dir = "{pinned}"\n', encoding="utf-8")
        result = run_install("--plan", env=self.env, cwd=self.home)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("source_dir", result.stderr)
        self.assertFalse((self.home / "Projects" / "recall").exists())
        self.assertNotIn("clone", result.stdout.lower())


if __name__ == "__main__":
    unittest.main()
