"""Re-export the one git-variable scrub. The names live in scripts/git_env.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path

_PATH = Path(__file__).resolve().parents[1] / "scripts" / "git_env.py"
_SPEC = importlib.util.spec_from_file_location("recall_scripts_git_env", _PATH)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"could not load {_PATH}")
_MODULE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_MODULE)

EXACT = _MODULE.EXACT
PREFIXES = _MODULE.PREFIXES
without_hook_git = _MODULE.scrub
