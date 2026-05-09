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
- combined timestamped transcript timeline

Still in progress:

- automatic transcription when a TUI session ends
- transcript dedupe when the mic also hears speaker audio
- persisted TUI markers/manual notes
- summary, decisions, and action-item extraction
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

See [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) for Homebrew and no-Brew setup paths.

## Install

Clone the repo, then:

```sh
cargo install --path .
```

Or run without installing:

```sh
cargo run
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
  whisper/
    bin/
      whisper-cli
models/
  ggml-base.en.bin
```

Then:

```sh
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
```

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

After ending a session, transcribe it:

```sh
recall transcribe latest
```

Open the latest session folder:

```sh
open "$(recall show latest)"
```

## TUI Keys

- `c`: toggle consent noted
- `Enter`: start recording
- `e`: end and finalize recording
- `r`: refresh detected sources
- `m`: add marker placeholder
- `n`: add manual note placeholder
- `q`: quit

Pause/resume is not wired for active real recording yet.

## Session Files

Recall writes sessions under `sessions/`:

```text
sessions/
  20260508-234814-project-sync/
    recall.json
    transcript.md
    summary.md
    actions.md
    audio/
      mic.m4a
      call.m4a
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
- [docs/FIRST_SESSION_TEST.md](docs/FIRST_SESSION_TEST.md): first real-call validation
- [docs/TRANSCRIPTION.md](docs/TRANSCRIPTION.md): transcription behavior and limitations
- [docs/AUDIO_CAPTURE.md](docs/AUDIO_CAPTURE.md): macOS audio capture notes
- [docs/PORTABILITY.md](docs/PORTABILITY.md): moving the project directory
- [docs/ROADMAP.md](docs/ROADMAP.md): milestone plan
- [docs/SPEC.md](docs/SPEC.md): product scope

## Consent

Recall is intended for consent-aware local recording.
