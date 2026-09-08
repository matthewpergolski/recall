---
name: push-main
description: >-
  Push Recall to origin/main with a crate version evaluated from git, not memory.
  Use when the user says push to main, ship, release, bump version, what version
  is next, or runs /push-main.
---

# Push Recall to main

Ship one intended change set to `origin/main` and set `Cargo.toml` `version` from **`origin/main`**, not from conversation memory.

`recall --version` is `CARGO_PKG_VERSION`. Git history on `main` stayed `0.1.0` through resume, Parakeet, continue-session, and other user-facing commits. Installed freshness is still the embedded git commit; the crate version is what humans read. Bump rules live in `AGENTS.md` (Versioning). This skill only computes the number and runs the push.

## Mode

- Default `/push-main` or "what version would this be?" → **plan only**. Print the table. Do not commit or push.
- `/push-main push` or the user said to push/ship this change now → plan, then execute if checks pass.

Do not `cargo install`, do not run `recall update`, and do not push a different worktree's commits.

## 1. Identify the checkout

```sh
git fetch origin
git status -sb
git rev-parse --show-toplevel
git branch --show-current
git worktree list
```

Stop if:

- This is not the Recall repo.
- The branch is not `main` and the user did not ask to merge this branch into `main`.
- Another worktree holds a **different** feature that would get mixed into this commit (for example append/new on `main` vs note-image-paste on a worktree). One user-facing push = one version = one feature set.
- The working tree has files that are not part of the intended ship. Stage explicitly; do not `git add -A`.

## 2. Evaluate version from git

Run this skill's script from the checkout you will ship:

```sh
python3 .grok/skills/push-main/scripts/next-version.py
```

Then classify the **intended** ship against `AGENTS.md` Versioning and re-run with `--kind`:

```sh
python3 .grok/skills/push-main/scripts/next-version.py --kind feature
python3 .grok/skills/push-main/scripts/next-version.py --kind patch
python3 .grok/skills/push-main/scripts/next-version.py --kind none
```

| `--kind` | When | Number |
|---|---|---|
| `feature` | New TUI/CLI behavior people will notice | `next_feature` (minor, patch resets to 0) |
| `patch` | Fix or user-visible docs correction | `next_patch` |
| `none` | Agent-only notes, tests-only, or the version is already correct for this ship | keep `released` |

`released` is `origin/main`'s `Cargo.toml`. `working` is this checkout. After a feature ships, the next feature is the **new** `released` plus one minor (0.2.0 on main → next feature 0.3.0). Two features in the same push share one bump; two features in two pushes are two bumps.

`--kind` status:

- `needs_bump` — set `Cargo.toml` (and refresh `Cargo.lock` via `cargo check`) to `proposed` in the **same** commit as the feature.
- `already_bumped` — working tree already matches `proposed`; keep it.
- `mismatch` — stop and explain. Typical cases: worktree still at 0.1.0 after main is 0.2.0; or a bump that does not match the kind.
- `ok_no_bump` — `--kind none` and working equals released.

Do not invent 1.0. Do not add a changelog unless asked. Do not use git tags unless asked.

## 3. Plan table (always)

Print this before any commit:

- checkout path and branch
- files that would ship vs `origin/main`
- `--kind` and script JSON (`released`, `working`, `proposed`, `status`)
- verify commands you will run
- commit subject (include the version when bumping)

Wait for push mode before executing.

## 4. Verify

Follow **Verification** in `AGENTS.md` for the files in this ship (Rust, docs-only, and/or Swift helper). For capture/resume/TUI session-mode changes, also follow that file's Ghostty TUI smoke and feature-capture rules (temp `--storage`, short recording, delete temp session, do not touch other Ghostty tabs).

Stop on failure.

## 5. Execute (push mode only)

1. Apply `proposed` to `Cargo.toml` when status is `needs_bump`. Run `cargo check` so `Cargo.lock`'s `name = "recall"` version matches.
2. Stage **only** the intended files plus the version files if bumped.
3. Commit. If bumping, the message names the version (example: `Add TUI append/new session mode and 0.2.0 version`).
4. `git push origin main`. If this checkout is a feature branch, fast-forward `main` to it after rebase onto `origin/main`; do not force-push `main`.
5. Report: `git log -1 --oneline`, `released` → `working`, and that the installed `~/.cargo/bin/recall` was **not** refreshed unless the user asked.

Do not commit `sessions/`, `memory-bank/`, `models/`, `tools/`, or build output.
