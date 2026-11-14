#!/usr/bin/env python3
"""The git variables a hook exports, and how to drop them.

Hooks, check.sh, and the Python git helpers all use this list. The shell
snippet scripts/git_env.sh prints nothing of its own; it evals `--unset`.
"""

from __future__ import annotations

import os
import sys

EXACT = (
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
)
PREFIXES = ("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")


def scrub(env: dict[str, str] | None = None) -> dict[str, str]:
    """A copy of env with hook git variables removed."""
    base = dict(os.environ if env is None else env)
    for key in list(base):
        if key in EXACT or key.startswith(PREFIXES):
            base.pop(key, None)
    return base


def unset_shell() -> str:
    """Bash that unsets the same variables. Hooks eval this."""
    names = " ".join(EXACT)
    pattern = "|".join(f"{prefix}*" for prefix in PREFIXES)
    return (
        f"unset {names}\n"
        "while IFS='=' read -r name _; do\n"
        '  case "$name" in\n'
        f'    {pattern}) unset "$name" ;;\n'
        "  esac\n"
        "done < <(env)\n"
    )


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if args == ["--unset"]:
        sys.stdout.write(unset_shell())
        return 0
    print("usage: git_env.py --unset", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
