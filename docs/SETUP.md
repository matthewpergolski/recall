# Setup

## Fresh Clone

```sh
git clone https://github.com/matthewpergolski/recall.git
cd recall
cargo install --path . --locked
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
cargo run -- --resume
cargo run -- --resume latest
cargo run -- start --title "Design Sync" --consent verbal
cargo run -- list
cargo run -- show latest
cargo run -- open latest
cargo run -- export latest
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
recall --resume
recall --resume latest
recall start --title "Design Sync" --consent verbal
recall list
recall show latest
recall open latest
recall export latest
recall sources
recall audio-tap-probe
recall transcribe latest
recall update
```

Install the local binary:

```sh
uv tool install parakeet-mlx
cargo install --path . --locked
```

`uv tool install parakeet-mlx` is required for the default Apple Silicon transcription engine. `recall update` does not install it. Whisper remains available with `--engine whisper` if you already have `whisper-cli`.

The first Parakeet run may download `mlx-community/parakeet-tdt-0.6b-v3` from Hugging Face (NVIDIA CC-BY-4.0).

## Updating An Installed Checkout

After the initial clone and install, run this from any directory:

```sh
recall update
```

The updater uses the checkout embedded in the installed binary, then falls back to `RECALL_REPO`, configured `source_dir`, the current directory, and common source-code locations. It verifies the official Git remote and requires a clean `main` branch tracking `origin/main`. An already-current checkout and executable produce one short message; an available update is fast-forwarded, tested, built, and installed with compact progress output.

If the checkout moved or Recall finds more than one valid clone, identify it explicitly:

```sh
recall update --repo ~/Projects/recall
```

Recall never resets, stashes, or discards local changes during an update.

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

Example persistent config:

```toml
consent_default = "provided"
storage_dir = "~/Documents/Recall/sessions"
source_dir = "~/Projects/recall"

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"

[transcription]
engine = "parakeet"
ffmpeg_bin = "~/Documents/Recall/tools/ffmpeg/bin/ffmpeg"
whisper_bin = "~/Documents/Recall/tools/whisper/bin/whisper-cli"
model_path = "~/Documents/Recall/models/ggml-base.en.bin"
parakeet_bin = "parakeet-mlx"
parakeet_model = "mlx-community/parakeet-tdt-0.6b-v3"
chunk_seconds = 600
```

## macOS Permissions

Recall needs macOS permission for:

- Microphone
- System audio recording

macOS attributes these permissions to the application that launches Recall, not to the `recall` command as a standalone app. Apple Terminal, Ghostty, VS Code, and Codex therefore have independent permission entries. If Recall works in Apple Terminal but records silent system audio in Ghostty, enable Ghostty under:

```text
System Settings -> Privacy & Security -> Screen & System Audio Recording
                -> System Audio Recording Only
```

Also enable the same launcher under **Microphone**. If it is missing from either list, use the `+` control to add the application. Fully quit and reopen the launcher after changing permission; opening a new shell tab is not always sufficient.

The default system/call path uses CoreAudio process taps and should use the narrower **System Audio Recording Only** permission. ScreenCaptureKit remains a fallback and may trigger the broader **Screen & System Audio Recording** prompt.

Validate access by playing a short video or macOS sound while Recall records. The **Call** meter should move. A process tap may start and create `audio/call.m4a` even when the resulting track is silent, so `recall audio-tap-probe` and file existence are not complete signal tests.

## Swift Helper Commands

```sh
cd capture-helper
swift build
swift run recall-capture list-sources
swift run recall-capture record-mic --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture record-audio-tap --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture record-system --session-dir ../sessions/<session-id> --duration 5
swift run recall-capture probe-audio-tap
swift run recall-capture clipboard-image --out /tmp/recall-clipboard.png
swift run recall-capture clipboard-text
```

`record-mic` writes microphone audio to `<session-dir>/audio/mic.m4a`.
`record-audio-tap` writes system/call audio to `<session-dir>/audio/call.m4a` through CoreAudio process taps.
`record-system` is the ScreenCaptureKit fallback and may require broader Screen Recording permission.
`probe-audio-tap` checks whether CoreAudio process taps are available.
`clipboard-image` writes the macOS pasteboard image as PNG; it exits 1 if the clipboard has no image.
`clipboard-text` emits the pasteboard string as JSON for Cmd+V text fallback after an image miss.

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
