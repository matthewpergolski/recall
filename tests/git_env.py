"""Drop git variables that a hook sets, so a command uses the repo it was given."""

from __future__ import annotations

import os

HOOK_GIT_VARS = (
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
)


def without_hook_git(env: dict[str, str] | None = None) -> dict[str, str]:
    base = dict(os.environ if env is None else env)
    for key in list(base):
        if key in HOOK_GIT_VARS or key == "GIT_CONFIG_PARAMETERS" or key == "GIT_CONFIG_COUNT":
            base.pop(key, None)
            continue
        if key.startswith("GIT_CONFIG_KEY_") or key.startswith("GIT_CONFIG_VALUE_"):
            base.pop(key, None)
    return base
