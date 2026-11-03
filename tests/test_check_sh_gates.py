"""scripts/check.sh must run identity and optional PR-body checks (F1)."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHECK_SH = ROOT / "scripts" / "check.sh"


class CheckShGatesTests(unittest.TestCase):
    def test_check_sh_runs_identity_on_origin_main_range(self) -> None:
        """F1: check.sh invokes check_identity.py on origin/main..HEAD."""
        text = CHECK_SH.read_text(encoding="utf-8")
        self.assertRegex(
            text,
            re.compile(
                r"python3\s+scripts/check_identity\.py\s+origin/main\.\.HEAD",
            ),
            "check.sh must run: python3 scripts/check_identity.py origin/main..HEAD",
        )

    def test_check_sh_runs_pr_body_when_body_file_given(self) -> None:
        """F1: check.sh runs check_pr_body.py when PR_BODY_FILE is set."""
        text = CHECK_SH.read_text(encoding="utf-8")
        self.assertIn("PR_BODY_FILE", text)
        self.assertRegex(
            text,
            re.compile(r"python3\s+scripts/check_pr_body\.py"),
            "check.sh must invoke scripts/check_pr_body.py when a body file is given",
        )


if __name__ == "__main__":
    unittest.main()
