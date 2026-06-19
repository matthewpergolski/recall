# Agent Instructions

These instructions are for Codex and other coding agents working on Recall.

## Purpose

Recall is a local-first macOS terminal app for meeting memory. It captures microphone and system/call audio, transcribes locally when possible, and can optionally generate summaries/actions with a headless CLI agent.

## Always Follow

- Keep Recall local-first and free-first.
- Do not add stealth recording behavior, permission bypasses, disguised capture flows, or hidden background capture.
- Do not commit private meeting data, model files, local tool binaries, build output, or memory-bank files.
- Use `rg` / `rg --files` for search.
- Use `apply_patch` for manual edits.
- Do not revert user changes unless explicitly asked.
- Keep changes scoped and incremental.

## Progressive Context

Read only the docs relevant to the task:

- Product scope: `docs/SPEC.md`
- Setup and first run: `docs/SETUP.md`
- Dependency sources and no-Brew setup: `docs/DEPENDENCIES.md`
- macOS audio capture: `docs/AUDIO_CAPTURE.md`
- Transcription behavior and limits: `docs/TRANSCRIPTION.md`
- Headless agent summaries/actions: `docs/AGENT_ANALYSIS.md`
- Moving the repo or installed binary: `docs/PORTABILITY.md`
- First real-call validation: `docs/FIRST_SESSION_TEST.md`
- Current roadmap: `docs/ROADMAP.md`

Public/user-facing documentation belongs in `README.md` and `docs/`.

## Memory Bank

The local `memory-bank/` is ignored by Git. It may contain private testing details and project-continuity state.

Before non-trivial code changes, read these files if they exist:

- `memory-bank/projectbrief.md`
- `memory-bank/productContext.md`
- `memory-bank/activeContext.md`
- `memory-bank/systemPatterns.md`
- `memory-bank/techContext.md`
- `memory-bank/progress.md`

Update the memory bank after major implementation, architecture, setup, dependency, or product-direction changes.

## Architecture

- Rust owns CLI, TUI, session state, config, transcription orchestration, and analysis orchestration.
- Swift helper owns macOS-specific capture APIs.
- `ratatui` + `crossterm` power the terminal UI.
- Session artifacts are plain local files.
- Generated private artifacts live under `sessions/`.
- Local models and tool binaries live under `models/` and `tools/`.

## Verification

For Rust changes, run:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy -- -D warnings
```

For docs-only changes, run:

```sh
git diff --check
```

For Swift helper changes, run from `capture-helper/`:

```sh
swift build
swift run recall-capture list-sources
```

In sandboxed Codex sessions, SwiftPM may need elevated execution because it uses normal macOS sandbox/cache paths.

## Skills

Codex skills are reusable capabilities installed outside this repo, commonly under the user's Codex skills directory. They are not the same thing as `AGENTS.md` and do not belong in `.agents/` by default.

Use relevant installed skills only when the task matches them. If this project later needs a custom reusable skill, create it intentionally as a Codex skill and keep project-specific continuity in `memory-bank/`.
