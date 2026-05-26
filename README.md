# Recall

Recall is a local-first macOS terminal app for recording meeting audio and turning it into transcripts and useful notes.

It records two local tracks:

- microphone audio: `audio/mic.m4a`
- meeting/system audio: `audio/call.m4a`

Transcription is local via `whisper.cpp`. Cloud transcription and hosted LLM services are not required.

## Status

Recall is usable as a macOS prototype:

- interactive Rust TUI
- consent-aware session start
- microphone recording
- CoreAudio system/call audio recording
- local session folders
- local Whisper transcription command
- chunked local transcription
- automatic transcription after ending a TUI session
- clean timestamped transcript for summary input
- debug transcript artifacts for raw timelines and per-track text
- optional headless agent analysis for summaries/actions

Still in progress:

- transcript dedupe tuning when the mic also hears speaker audio
- persisted TUI markers/manual notes
- real-agent output validation and prompt tuning
- packaging/distribution

## Requirements

Required for the app:

- macOS
- Rust + Cargo
- Swift toolchain / Xcode Command Line Tools

Required for transcription:

- `whisper-cli` from `whisper.cpp`
- a `ggml` Whisper model file
- `ffmpeg` for now, used to convert `.m4a` to Whisper-ready `.wav`

Optional for agent analysis:

- one supported headless CLI agent installed and authenticated, such as `grok`, `cline`, `codex`, or `claude`

See [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) for Homebrew and no-Brew setup paths.

## Fresh Clone Quickstart

Clone the repo:

```sh
git clone https://github.com/matthewpergolski/recall.git
cd recall
```

Install the local `recall` command:

```sh
cargo install --path .
```

Or run from the repo without installing:

```sh
cargo run
```

Check the local setup:

```sh
recall doctor
recall sources
recall audio-tap-probe
```

## Transcription Setup

Fast personal Mac setup:

```sh
brew install whisper-cpp
mkdir -p models
curl -L -o models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

Corporate/no-Brew setup:

```text
tools/
  ffmpeg/
    bin/
      ffmpeg
  whisper/
    bin/
      whisper-cli
models/
  ggml-base.en.bin
```

Then:

```sh
export PATH="$PWD/tools/ffmpeg/bin:$PATH"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
```

The model can come from Hugging Face. The `whisper-cli` and `ffmpeg` binaries should come from source builds, release artifacts, or internal binaries approved by your organization.

## Usage

Start the TUI:

```sh
recall
```

Start with consent already marked:

```sh
recall --consent provided
```

Start with a session title:

```sh
recall --title "Project sync"
```

When you press `e` in the TUI, Recall finalizes audio and starts local transcription automatically. The TUI shows transcript progress and the output path when ready.

You can also transcribe manually:

```sh
recall transcribe latest
```

## Agent Analysis

Recall can hand the clean `transcript.md` to a headless coding agent and write summary/action files.

See [docs/AGENT_ANALYSIS.md](docs/AGENT_ANALYSIS.md) for all setup modes: CLI flags, alias, and config defaults.

When the agent returns a useful title, Recall updates generic session headings such as `Quick Capture` in the generated Markdown/session metadata and renames the session folder with a topic-based slug.

Supported built-in agent profiles:

- `grok`
- `cline`
- `codex`
- `claude`

Run analysis manually:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
```

Preview the generated prompt without running an agent:

```sh
recall analyze latest --agent grok --dry-run
```

Enable automatic analysis after transcription:

```sh
recall --agent grok --auto-analyze
```

Outputs:

```text
summary.md
actions.md
decisions.md
questions.md
followups.md
analysis-debug/
  prompt.md
  agent-raw-output.json or agent-raw-output.jsonl
  agent-result.json
```

Optional personal alias:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

Optional config file:

```toml
# ~/.config/recall/config.toml
consent_default = "provided"

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"
```

For long recordings, Recall chunks each audio track before transcription. The default chunk size is 10 minutes:

```sh
recall transcribe latest --chunk-seconds 600
```

Open the latest session folder:

```sh
open "$(recall show latest)"
```

## TUI Keys

- `c`: toggle consent noted
- `Enter`: start recording
- `e`: end and finalize recording
- transcript progress starts after `e`
- `r`: refresh detected sources
- `m`: add marker placeholder
- `n`: add manual note placeholder
- `q` or `Ctrl+C`: quit

Pause/resume is not wired for active real recording yet.

## Session Files

Recall writes sessions under `sessions/`:

```text
sessions/
  05-26-2026_7-21pm-et-project-sync/
    recall.json
    transcript.md
    summary.md
    actions.md
    decisions.md
    questions.md
    followups.md
    audio/
      mic.m4a
      call.m4a
    transcription-debug/
      combined-timeline.md
      raw-tracks.md
      full-debug-transcript.md
    analysis-debug/
      prompt.md
      agent-raw-output.json or agent-raw-output.jsonl
      agent-result.json
```

`sessions/` is ignored by Git because it contains private meeting data.

## Useful Commands

```sh
recall
recall --title "Project sync"
recall list
recall show latest
recall sources
recall audio-tap-probe
recall transcribe latest
recall transcribe latest --track call
recall transcribe latest --track mic
recall analyze latest --agent grok
recall agents list
recall agents doctor
recall doctor
```

Development commands:

```sh
cargo fmt
cargo check
cargo test
cargo clippy
```

Swift helper commands:

```sh
cd capture-helper
swift build
swift run recall-capture list-sources
swift run recall-capture record-mic --session-dir ../sessions/example --duration 5
swift run recall-capture record-audio-tap --session-dir ../sessions/example --duration 5
swift run recall-capture probe-audio-tap
```

## Privacy

Do not commit generated meeting data, model files, or build output.

Ignored by default:

- `sessions/`
- `models/`
- `tools/`
- `target/`
- `capture-helper/.build/`
- `memory-bank/`

See [docs/GIT_PRIVACY_CHECKLIST.md](docs/GIT_PRIVACY_CHECKLIST.md).

## License

Copyright (c) 2026 Matthew Pergolski. All rights reserved.

This repository is public for portfolio and review purposes only. No license is granted for reuse, redistribution, or derivative works.

## Documentation

- [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md): dependency sources and no-Brew setup
- [docs/AGENT_ANALYSIS.md](docs/AGENT_ANALYSIS.md): headless agent setup and config
- [docs/FIRST_SESSION_TEST.md](docs/FIRST_SESSION_TEST.md): first real-call validation
- [docs/TRANSCRIPTION.md](docs/TRANSCRIPTION.md): transcription behavior and limitations
- [docs/AUDIO_CAPTURE.md](docs/AUDIO_CAPTURE.md): macOS audio capture notes
- [docs/PORTABILITY.md](docs/PORTABILITY.md): moving the project directory
- [docs/ROADMAP.md](docs/ROADMAP.md): milestone plan
- [docs/SPEC.md](docs/SPEC.md): product scope

## Consent

Recall is intended for consent-aware local recording.
