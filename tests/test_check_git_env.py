"""Hooks and scripts share one git-variable list."""

from __future__ import annotations

import importlib.util
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load_git_env():
    path = ROOT / "scripts" / "git_env.py"
    spec = importlib.util.spec_from_file_location("recall_scripts_git_env", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SharedGitEnvTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.git_env = load_git_env()

    def test_hooks_and_scripts_source_the_shared_list(self) -> None:
        for name in (
            ".githooks/commit-msg",
            ".githooks/pre-commit",
            ".githooks/pre-push",
            "scripts/check.sh",
        ):
            text = (ROOT / name).read_text(encoding="utf-8")
            self.assertIn("scripts/git_env.sh", text, name)
            self.assertNotIn("unset GIT_DIR", text, name)
        snippet = (ROOT / "scripts" / "git_env.sh").read_text(encoding="utf-8")
        self.assertIn("git_env.py", snippet)
        self.assertNotIn("GIT_DIR", snippet)
        for name in (
            "check_identity.py",
            "check_version_bump.py",
            "versions.py",
        ):
            text = (ROOT / "scripts" / name).read_text(encoding="utf-8")
            self.assertIn("git_env.py", text, name)
            self.assertNotIn('"GIT_DIR"', text, name)

    def test_shell_snippet_unsets_the_python_list(self) -> None:
        exports = "\n".join(
            f"export {name}=leak" for name in (*self.git_env.EXACT, "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0")
        )
        checks = "\n".join(
            f'if [ -n "${{{name}+x}}" ]; then echo "still {name}"; exit 1; fi'
            for name in (*self.git_env.EXACT, "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0")
        )
        script = f"set -euo pipefail\n{exports}\nsource scripts/git_env.sh\n{checks}\necho clean\n"
        result = subprocess.run(
            ["bash", "-c", script],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("clean", result.stdout)

    def test_scrub_drops_exact_names_and_config_prefixes(self) -> None:
        env = {"PATH": "/bin", "GIT_CONFIG_KEY_0": "core.bare", "GIT_CONFIG_VALUE_0": "true"}
        for name in self.git_env.EXACT:
            env[name] = "leak"
        cleaned = self.git_env.scrub(env)
        self.assertEqual(cleaned, {"PATH": "/bin"})

    def test_build_rs_scrubs_with_the_same_names(self) -> None:
        build = (ROOT / "build.rs").read_text(encoding="utf-8")
        self.assertIn('include!("git_hook_env.rs")', build)
        self.assertIn("scrub_git_command", build)
        self.assertEqual(build.count('Command::new("git")'), 1)
        rust = (ROOT / "git_hook_env.rs").read_text(encoding="utf-8")
        for name in self.git_env.EXACT:
            self.assertIn(f'"{name}"', rust, name)
        for prefix in self.git_env.PREFIXES:
            self.assertIn(f'"{prefix}"', rust, prefix)
