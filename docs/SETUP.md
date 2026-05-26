# Setup

## Fresh Clone

```sh
git clone https://github.com/matthewpergolski/recall.git
cd recall
cargo install --path .
```

Or run without installing:

```sh
cargo run
```

Run setup checks:

```sh
recall doctor
recall sources
recall audio-tap-probe
```

## Required Toolchain

Recall currently needs:

- Rust compiler: `rustc`
- Rust package manager: `cargo`
- Swift toolchain: `swift`
- macOS with Xcode Command Line Tools

Run:

```sh
rustc --version
cargo --version
swift --version
```

## Rust Mental Model for Python Users

Python:

```sh
uv run python app.py
uv add rich
pytest
```

Rust:

```sh
cargo run
cargo add ratatui
cargo test
```

Mapping:

- `pyproject.toml` is similar to `Cargo.toml`
- `.venv` usually has no Rust equivalent
- `uv add` is similar to `cargo add`
- `uv run` is similar to `cargo run`
- `pytest` is similar to `cargo test`
- `ruff`/formatting is similar to `cargo fmt`
- static checks are commonly done with `cargo check` and `cargo clippy`

## Rust Version

Recall currently targets Rust `1.95` in `Cargo.toml`.

If your local version is older, update it:

```sh
rustup update stable
rustup default stable
```

Then verify:

```sh
rustc --version
cargo --version
```

## First Commands

```sh
cargo run
cargo run -- --consent provided
cargo run -- --title "Project sync"
cargo run -- start --title "Design Sync" --consent verbal
cargo run -- list
cargo run -- show latest
cargo run -- sources
cargo run -- audio-tap-probe
cargo run -- transcribe latest
cargo run -- doctor
cargo check
```

When the app is installed as a binary, those become:

```sh
recall
recall --consent provided
recall --title "Project sync"
recall start --title "Design Sync" --consent verbal
recall list
recall show latest
recall sources
recall audio-tap-probe
recall transcribe latest
```

Install the local binary:

```sh
cargo install --path .
```

Optional shell shortcut:

```sh
alias recall-ready='recall --consent provided'
```

Use `recall-ready` when you want to launch the TUI with consent already marked.

You can also alias `recall` itself with defaults:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

Recall parses leading defaults before subcommands, so `recall list`, `recall sources`, and `recall transcribe latest` still work. For persistent defaults without shell aliases, use `~/.config/recall/config.toml`; see `docs/AGENT_ANALYSIS.md`.

## macOS Permissions

The real capture implementation will need macOS permissions:

- Microphone
- System/call audio capture permissions as required by macOS

The default system/call audio path uses CoreAudio process taps. ScreenCaptureKit remains as a fallback and may trigger broader Screen Recording prompts. Prefer testing from a dedicated terminal app so permissions are scoped to that launcher rather than an IDE.

## Swift Helper Commands

```sh
cd capture-helper
swift build
swift run recall-capture list-sources
swift run recall-capture record-mic --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture record-audio-tap --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture record-system --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture probe-audio-tap
```

`record-mic` writes microphone audio to `<session-dir>/audio/mic.m4a`.
`record-audio-tap` writes system/call audio to `<session-dir>/audio/call.m4a` through CoreAudio process taps.
`record-system` is the ScreenCaptureKit fallback and may require broader Screen Recording permission.
`probe-audio-tap` checks whether CoreAudio process taps are available.

## Current Rust Dependencies

Already added:

- `ratatui`
- `crossterm`
- `serde`
- `serde_json`
- `time`

Likely later additions:

- `tokio` for async jobs
- `anyhow` for richer error handling
