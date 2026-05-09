# Agent Instructions

These instructions are for Codex and other coding agents working on Recall.

## Memory Bank

Agents should maintain a local memory bank for project continuity. The memory bank is intentionally ignored by Git because it can contain local testing details, private session IDs, and user-specific context.

Before making changes, read all core memory bank files if they exist:

- `memory-bank/projectbrief.md`
- `memory-bank/productContext.md`
- `memory-bank/activeContext.md`
- `memory-bank/systemPatterns.md`
- `memory-bank/techContext.md`
- `memory-bank/progress.md`

If they do not exist, initialize them when project continuity would benefit from it. Keep them focused and factual.

Treat the memory bank as durable local project state. Update it after:

- significant implementation changes
- major product or architecture decisions
- dependency/setup changes
- explicit user requests to update memory
- discoveries that future agents would otherwise have to rediscover

Do not rely on the memory bank as public documentation. Public/user-facing documentation belongs in `README.md` and `docs/`.

## Project Direction

Recall is a local-first macOS terminal app for meeting memory.

Core direction:

- Rust owns CLI, TUI, session state, and local storage.
- Swift helper owns macOS-specific capture APIs.
- `ratatui` + `crossterm` power the terminal UI.
- Meeting artifacts should remain understandable local files.
- Keep the product useful for general users, not only developers.
- Keep developer/repo-aware behavior optional and later.

## Cost Policy

Build free-first.

- Prefer local capture.
- Prefer local files.
- Prefer local/open-source transcription options.
- Do not make paid cloud transcription or hosted LLM summarization required.
- Any paid service integration must be optional.

## Skills

Codex skills are reusable capabilities installed outside this repo, commonly under the user's Codex skills directory. They are not the same thing as `AGENTS.md` and do not belong in `.agents/` by default.

Use relevant installed skills when the task matches them. For this project, likely useful skills include:

- `skill-creator`: only if the user wants to create a new reusable Codex skill.
- `skill-installer`: only if the user wants to install a Codex skill.
- `browser-use:browser`: if validating a local web UI or browser target. This project is currently terminal-native, so it is usually not needed.

If this project later needs a custom reusable skill, create it intentionally as a Codex skill, not as ad hoc project documentation. Keep project-specific continuity in `memory-bank/`.

## Repo Conventions

- Use `rg` / `rg --files` for searching.
- Use `apply_patch` for manual edits.
- Do not revert user changes.
- Keep generated session output under `sessions/`; these are ignored by git.
- Keep implementation scoped and incremental.

## Verification

For Rust changes, run:

```sh
cargo fmt
cargo check
cargo test
```

Run `cargo clippy` when changing non-trivial Rust logic.

For the Swift helper, run from `capture-helper/`:

```sh
swift build
swift run recall-capture list-sources
```

In Codex sandboxed sessions, SwiftPM may need elevated execution because it uses normal macOS sandbox/cache paths.

## Portability

If the project is moved to a new directory, read `docs/PORTABILITY.md`. Re-run `cargo check`, `swift build`, and reinstall with `cargo install --path .` if the user relies on a global `recall` command.
