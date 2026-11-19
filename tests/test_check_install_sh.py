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
        first = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
        self.assertIn("plan:", first.stdout)
        second = run_install("--plan", "--repo", str(checkout), env=self.env)
        self.assertEqual(second.returncode, 0, second.stderr + second.stdout)
        self.assertIn("nothing else to change", second.stdout)

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
        # Empty home: no checkout. Plan should mention clone into Projects/recall.
        result = run_install("--plan", env=self.env, cwd=self.home)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertIn("clone", result.stdout.lower())
        self.assertIn("Projects/recall", result.stdout)

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


if __name__ == "__main__":
    unittest.main()
