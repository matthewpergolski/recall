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
cargo install --path . --locked
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

The default transcription engine is Parakeet on Apple Silicon. Whisper remains a first-class fallback.

Default engine (Parakeet):

| Dependency | Purpose | Source |
| --- | --- | --- |
| `parakeet-mlx` | Default local speech-to-text CLI | `uv tool install parakeet-mlx` (preferred) or pip; Apache-2.0. Not Homebrew. |
| `mlx-community/parakeet-tdt-0.6b-v3` | Default Parakeet MLX weights | Hugging Face on first run; NVIDIA **CC-BY-4.0** (credit NVIDIA / the model) |
| `ffmpeg` | Chunks Recall `.m4a` audio and converts chunks to 16 kHz mono WAV | Homebrew for now, or an approved corporate binary |

Fallback engine (Whisper). Still supported, never removed:

| Dependency | Purpose | Source |
| --- | --- | --- |
| `whisper-cli` | Fallback local speech-to-text inference (`--engine whisper`) | `whisper.cpp` on GitHub |
| `ggml` Whisper model | Model weights loaded by `whisper-cli` | Hugging Face `ggerganov/whisper.cpp` |

`ffmpeg` is a temporary dependency. `recall update` does **not** install `parakeet-mlx`, `whisper-cli`, `ffmpeg`, or model weights. It only refreshes the Recall source checkout and the `recall` Cargo binary. Install ASR tools yourself, then keep them.

Missing `parakeet-mlx` does not break `--engine whisper`. `recall doctor` reports a missing Parakeet CLI as a warning. First Parakeet run may download weights; later runs can be offline once the cache is warm. Do not commit model files.

## Personal Mac Setup With Homebrew

Use this on a personal machine where Homebrew is allowed:

```sh
uv tool install parakeet-mlx
brew install whisper-cpp   # fallback engine only
mkdir -p models
curl -L -o models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
cargo install --path . --locked
recall transcribe latest
```

`uv tool install parakeet-mlx` puts `parakeet-mlx` on PATH. Homebrew is fine for `uv` or `ffmpeg`, but do not expect a `brew install parakeet-mlx` formula. The first `recall transcribe` downloads the MLX model from Hugging Face.

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
export RECALL_FFMPEG_BIN="$PWD/tools/ffmpeg/bin/ffmpeg"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
recall transcribe latest
```

Recall also auto-detects `tools/ffmpeg/bin/ffmpeg` when present. `RECALL_FFMPEG_BIN` is useful when the binary lives somewhere else or you do not want to modify `PATH`.

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

## Parakeet (`parakeet-mlx`)

Parakeet is the default engine. Install the CLI yourself; Recall will not install it during `recall update`.

```sh
uv tool install parakeet-mlx
# or: pip install parakeet-mlx
recall transcribe latest
recall transcribe latest --engine whisper   # fallback
```

Optional overrides:

```sh
export RECALL_PARAKEET_BIN=/path/to/parakeet-mlx
export RECALL_PARAKEET_MODEL=mlx-community/parakeet-tdt-0.6b-v3
export RECALL_PARAKEET_CACHE="$HOME/Library/Application Support/recall/models/parakeet"
```

The default model is NVIDIA Parakeet TDT 0.6B v3, converted for MLX. License: **CC-BY-4.0**. Commercial use is allowed with attribution. `recall doctor` and `recall spec` mention this. Weights stay in the Hugging Face cache or a configured Recall cache directory; they are gitignored.

## Optional Agent Analysis

Agent analysis is optional. Recording and local transcription work without it.

If you want Recall to generate a complete `meeting.md`, install and authenticate at least one supported headless CLI agent:

| Agent | Command Recall Expects |
| --- | --- |
| Grok | `grok` |
| Cline | `cline` |
| Codex | `codex` |
| Claude | `claude` |
| OpenCode | `opencode` |
| Pi | `pi` |

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
storage_dir = "~/Documents/Recall/sessions"

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

Headless agents may call their own hosted services depending on the tool. Keep this optional when you need a fully local-only workflow.

## Future Installer Direction

Potential future commands:

```sh
recall doctor transcription
recall setup transcription
```

`doctor` should report what is present or missing. `setup` should ask before downloading anything and should support corporate policies by allowing manual/offline placement.
