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

`cargo test` already runs unit tests and `tests/cli_smoke.rs`, which invokes the compiled `recall` binary. Use that (or `cargo run -- <args>`) to verify CLI behavior from this checkout. Do not ask the user to `cargo install` or start a live meeting just to prove flag parsing, resume errors, or other non-TUI paths.

The interactive TUI needs a real terminal. Agent command output is not a TTY (`TERM=dumb`, piped stdin/stdout), so `cargo run` cannot drive the dashboard from this shell.

For a TUI smoke of capture/resume/session-mode changes, drive **Ghostty** through its own AppleScript (not System Events): `new window with configuration` to run `target/debug/recall --storage <temp> --consent provided --no-auto-analyze`, then `focus` and `send key` with press+release. **Enter** starts/ends a take; **Space** was unreliable. Identify the window by the id returned at create time (title stays `👻`). Close only that window. Do not quit Ghostty or touch the user's other tabs. Screen Recording and Accessibility are optional; without them, infer state from the temp session folder and `pgrep`. This is Ghostty-specific. Apple Terminal can `do script` to launch a command, but has no `send key`. There is no generic “any terminal” API.

### Feature capture tests

When the task is a capture, resume, transcription, or recorder-path change, agents may run a **short, obvious, throwaway** mic/system recording to verify the feature. This is standing permission for that kind of work only. Do not record during docs-only, CLI-parse, or unrelated changes.

Rules:

- Use a temp `--session-dir` / `--storage` under the system temp directory. Never write test captures into the user's real `sessions/`.
- Prefer the duration-limited Swift helper, which does not need a TUI:

```sh
swift run --package-path capture-helper recall-capture record-mic --session-dir /tmp/recall-agent-smoke --duration 3
swift run --package-path capture-helper recall-capture record-audio-tap --session-dir /tmp/recall-agent-smoke --duration 3
```

- Keep it a few seconds. Do not leave recorders running.
- Do not hide, disguise, or background-capture after the test. Delete the temp session when finished.
- Do not commit those files. macOS may still prompt for Microphone / System Audio Recording; if permission is denied, report that and stop rather than bypassing it.

The installed `~/.cargo/bin/recall` is a separate binary. Source edits are verified with `cargo test` / `cargo run`; install only when the user wants their shell `recall` command refreshed.

## Versioning

`recall --version` and `recall update` copy `version` from `Cargo.toml` (`CARGO_PKG_VERSION`). That field has stayed `0.1.0` while git `main` moved. Up-to-date-ness of an installed binary is the embedded git commit, not the crate version.

On a user-facing push (new TUI/CLI behavior people will notice), bump `Cargo.toml` `version` in the same commit:

- `0.x.y` while the product is still pre-1.0
- **minor** (`0.2.0`) for features (resume, session append/new, capture changes)
- **patch** (`0.2.1`) for fixes and docs-only user-visible corrections

Do not bump for agent-only notes or test-only changes. Do not add a public changelog unless the user asks.

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

## Installed Command

Source edits do not automatically replace the user's installed `~/.cargo/bin/recall` command. Do not install, commit, push, or run the networked updater unless the user requests it.

For a clean end-user checkout after approved changes have been pushed, `recall update` verifies and updates `origin/main`, runs tests, builds the Swift helper, and reinstalls the command. During active development in a dirty checkout, verify normally and use `cargo install --path . --locked` only when the user asks to refresh the installed binary.

## Skills

Codex skills are reusable capabilities installed outside this repo, commonly under the user's Codex skills directory. They are not the same thing as `AGENTS.md` and do not belong in `.agents/` by default.

Use relevant installed skills only when the task matches them. If this project later needs a custom reusable skill, create it intentionally as a Codex skill and keep project-specific continuity in `memory-bank/`.
