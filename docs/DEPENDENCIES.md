# Dependencies

Recall should be usable in two modes:

1. Fast personal setup with Homebrew.
2. Corporate/no-Brew setup where approved binaries and model files are placed manually.

## Core App

Required to build and run Recall from source:

| Dependency | Purpose | Source |
| --- | --- | --- |
| Rust + Cargo | Builds the `recall` TUI app | `rustup` / approved corporate Rust install |
| Swift toolchain | Builds the macOS capture helper | Xcode Command Line Tools / approved corporate Xcode install |
| macOS | Initial supported capture platform | Apple |

Install the Recall command from this repo:

```sh
cargo install --path .
```

## Audio Capture

Audio capture currently uses Apple frameworks through the Swift helper:

| Dependency | Purpose | Source |
| --- | --- | --- |
| AVFoundation | Microphone capture and audio files | macOS SDK |
| CoreAudio process taps | System/call audio capture | macOS SDK |
| ScreenCaptureKit | Fallback system audio path | macOS SDK |

These are provided by macOS/Xcode. They are not downloaded from Homebrew or Hugging Face.

## Transcription

Transcription currently needs three things:

| Dependency | Purpose | Source |
| --- | --- | --- |
| `whisper-cli` | Runs local speech-to-text inference | `whisper.cpp` on GitHub |
| `ggml` Whisper model | Model weights loaded by `whisper-cli` | Hugging Face `ggerganov/whisper.cpp` |
| `ffmpeg` | Chunks Recall `.m4a` audio and converts chunks to 16 kHz mono WAV | Homebrew for now, or an approved corporate binary |

`ffmpeg` is a temporary dependency. The intended corporate-friendly direction is to replace it with a Swift/AVFoundation conversion command so transcription only needs `whisper-cli` and a model.

## Personal Mac Setup With Homebrew

Use this on a personal machine where Homebrew is allowed:

```sh
brew install whisper-cpp
mkdir -p models
curl -L -o models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
cargo install --path .
recall transcribe latest
```

## No-Brew Corporate Setup

Use this model when Homebrew is not allowed.

Expected project-local layout:

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

Where the files come from:

- `whisper-cli`: build or obtain an approved binary from `whisper.cpp`.
- `ggml-base.en.bin`: download from Hugging Face `ggerganov/whisper.cpp`.
- `ffmpeg`: use an approved corporate binary until Recall has native audio conversion.

Then point Recall at those files:

```sh
export PATH="$PWD/tools/ffmpeg/bin:$PATH"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
recall transcribe latest
```

Recall currently finds `ffmpeg` through `PATH`. Put an approved `ffmpeg` binary on `PATH` until Recall replaces that dependency with native AVFoundation conversion.

## Manual Model Download

Model source:

```text
https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

Command:

```sh
mkdir -p models
curl -L -o models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

The `base.en` model is a reasonable first default for English meetings. It is smaller and faster than `small` or `medium`, and much smaller than `large`.

## Manual `whisper-cli` Options

Options for getting `whisper-cli` without Homebrew:

1. Build `whisper.cpp` from source with approved developer tools.
2. Download an approved release artifact if your organization permits it.
3. Have IT/security publish an internally approved `whisper-cli` binary.

Recall should not assume Homebrew in corporate environments. It should accept explicit paths through `RECALL_WHISPER_BIN` and `RECALL_WHISPER_MODEL`.

## Optional Agent Analysis

Agent analysis is optional. Recording and local transcription work without it.

If you want Recall to generate summary/action files, install and authenticate at least one supported headless CLI agent:

| Agent | Command Recall Expects |
| --- | --- |
| Grok | `grok` |
| Cline | `cline` |
| Codex | `codex` |
| Claude | `claude` |

Check local availability:

```sh
recall agents list
recall agents doctor
```

Configure a default agent with CLI flags:

```sh
recall --agent grok --auto-analyze
recall analyze latest --agent grok
```

Or with local config:

```toml
# ~/.config/recall/config.toml
consent_default = "provided"

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"
```

Headless agents may call their own hosted services depending on the tool. Keep this optional when you need a fully local-only workflow.

## Future Installer Direction

Potential future commands:

```sh
recall doctor transcription
recall setup transcription
```

`doctor` should report what is present or missing. `setup` should ask before downloading anything and should support corporate policies by allowing manual/offline placement.
