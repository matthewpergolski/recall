# Recall

Recall is a local-first macOS terminal app for recording meeting audio and turning it into transcripts and useful notes.

It records two local tracks:

- microphone audio: `audio/mic.m4a`
- meeting/system audio: `audio/call.m4a`

Transcription is local. The default engine on Apple Silicon is NVIDIA Parakeet TDT 0.6B v3 via `parakeet-mlx`. Whisper/`whisper-cli` remains a full fallback. Cloud transcription and hosted LLM services are not required.

## Status

Recall is usable as a macOS prototype:

- interactive Rust TUI
- consent-aware session start
- microphone recording, including AirPods when that is the input at start
- CoreAudio system/call audio recording when the launching terminal has system-audio permission
- local session folders
- local Parakeet transcription command (`parakeet-mlx`, default)
- Whisper/`whisper-cli` fallback with `--engine whisper`
- chunked local transcription
- automatic transcription after ending a TUI session
- clean timestamped transcript for summary input
- debug transcript artifacts for raw timelines and per-track text
- optional headless agent analysis that produces one complete `meeting.md`
- typed TUI notes and timestamped markers retained with session internals
- one-command meeting opening and portable Markdown export
- configurable storage and transcription binary/model paths

Capture has been verified on real calls and on AirPods. Known limits, not missing tests:

- macOS permission belongs to the app that launched Recall. Apple Terminal, Ghostty, VS Code, and Codex are separate. A working session in Terminal does not imply Ghostty is granted.
- Watch the **Call** meter. A full-length `call.m4a` can still be digital silence if the launcher lacks system-audio access.
- Starting on AirPods records the mic for the whole session. Switching *to* AirPods mid-call is detected in the TUI. The mic file sometimes continues for the rest of the session and sometimes ends at the switch. Recall does not yet restart and stitch mic segments.
- Speaker-mode meetings can still duplicate remote speech on the mic track; clean-transcript dedupe is conservative.

Still in progress: packaging/distribution, and further transcript-merge tuning for speaker bleed.

## Requirements

Required for the app:

- macOS
- Rust + Cargo
- Swift toolchain / Xcode Command Line Tools

Required for transcription:

- `parakeet-mlx` (`uv tool install parakeet-mlx`; not Homebrew)
- `ffmpeg` for now, used to convert `.m4a` to 16 kHz mono `.wav`
- NVIDIA Parakeet TDT 0.6B v3 MLX weights (`mlx-community/parakeet-tdt-0.6b-v3`, CC-BY-4.0), downloaded on first run

Whisper fallback (`--engine whisper`):

- `whisper-cli` from `whisper.cpp`
- a `ggml` Whisper model file

Optional for agent analysis:

- one supported headless CLI agent installed and authenticated, such as `grok`, `cline`, `codex`, `claude`, `opencode`, or `pi`

See [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) for Homebrew and no-Brew setup paths.

## Fresh Clone Quickstart

Clone the repo:

```sh
git clone https://github.com/matthewpergolski/recall.git
cd recall
```

Install the local `recall` command:

```sh
cargo install --path . --locked
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

Before the first recording, grant **Microphone** and **System Audio Recording Only** access to the terminal application that will launch Recall. macOS scopes these permissions to the launcher: Apple Terminal, Ghostty, VS Code, and Codex are separate applications. Permission granted to one does not apply to the others. Fully quit and reopen the launcher after changing its permission.

During a short test, play system audio and confirm that Recall's **Call** meter moves. A created `call.m4a` file does not by itself prove that macOS supplied audible system audio.

After this first installation, update the source checkout and installed command from any directory:

```sh
recall update
```

Recall locates its verified source checkout and checks `origin/main`. If the checkout and installed executable already match, it exits with one short message. When an update exists, it fast-forwards safely, runs the Rust tests, builds the Swift helper, and reinstalls the command with compact progress output. Use `recall update --repo /path/to/recall` if automatic discovery cannot find the checkout.

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
export RECALL_FFMPEG_BIN="$PWD/tools/ffmpeg/bin/ffmpeg"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
```

The model can come from Hugging Face. The `whisper-cli` and `ffmpeg` binaries should come from source builds, release artifacts, or internal binaries approved by your organization.

Default Parakeet install (does not happen during `recall update`):

```sh
uv tool install parakeet-mlx
recall transcribe latest
```

First Parakeet run downloads `mlx-community/parakeet-tdt-0.6b-v3` (~1.2 GB) into the local Hugging Face cache. Recall shows that as a one-time download phase with size, rate, and cache path, then starts transcription. NVIDIA Parakeet TDT 0.6B v3 is CC-BY-4.0; credit NVIDIA / the model. `recall doctor` warns if `parakeet-mlx` is missing and still checks `whisper-cli`. Use `--engine whisper` for the Whisper fallback.

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

When you press Space or Enter to end a recording, Recall finalizes audio and starts local transcription automatically. The TUI shows transcript progress and the output path when ready. If you stay in that TUI session and press Space or Enter again, Recall continues the same meeting folder with a new audio take. Quitting (`q` / Ctrl+C) and launching Recall again starts a new session.

You can also transcribe manually:

```sh
recall transcribe latest
```

## Agent Analysis

Recall can hand the clean `transcript.md` to a headless coding agent and write one complete `meeting.md` containing the summary, decisions, action items, questions, follow-ups, notes, and markers.

See [docs/AGENT_ANALYSIS.md](docs/AGENT_ANALYSIS.md) for all setup modes: CLI flags, alias, and config defaults.

When the agent returns a useful title, Recall updates generic session headings such as `Quick Capture` in the generated Markdown/session metadata and renames the session folder with a topic-based slug.

Supported built-in agent profiles:

- `grok`
- `cline`
- `codex`
- `claude`
- `opencode`
- `pi`

Run analysis manually:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
recall analyze latest --agent opencode
recall analyze latest --agent pi
```

Preview the generated prompt without running an agent:

```sh
recall analyze latest --agent grok --dry-run
```

Enable automatic analysis after transcription:

```sh
recall --agent grok
```

Outputs:

```text
meeting.md
.recall/analysis/
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
storage_dir = "~/Documents/Recall/sessions"
source_dir = "~/Projects/recall"
editor = "code"

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

For long recordings, Recall chunks each audio track before transcription. The default chunk size is 10 minutes:

```sh
recall transcribe latest --chunk-seconds 600
```

Open the latest meeting document:

```sh
recall open latest
recall open latest --dir
recall open latest --dir --editor code
```

`recall open latest` opens `meeting.md` in the default macOS handler. `--dir` opens the session folder. Set `editor = "code"` in `~/.config/recall/config.toml`, or `RECALL_EDITOR=code`, to open that folder in VS Code (or `cursor`, `zed`, or an app name such as `"Visual Studio Code"`). Without an editor setting, macOS Finder opens the folder.

Create one portable Markdown file containing the meeting record and full transcript:

```sh
recall export latest
recall export latest --output ~/Desktop/project-sync.md
```

## TUI Keys

- `c`: toggle consent noted
- `Space` or `Enter`: start recording when idle; end and finalize when recording; continue the same session after it ends
- transcript progress starts after recording ends; a continued take re-transcribes the full concatenated audio
- `r`: refresh detected sources
- `m`: add a timestamped marker to the session
- `n`: type a timestamped note, then `Enter` saves it with the session
- `o`: open the session folder in Finder or the configured editor
- `O`: open the meeting document
- `q` or `Ctrl+C`: quit

Pause/resume without ending the take is still disabled. Ending finalizes the current `m4a` files so transcription can start; continuing writes the next numbered take into the same session.

## Session Files

Recall writes sessions under `sessions/`:

```text
sessions/
  05-26-2026_7-21pm-et-project-sync/
    meeting.md
    transcript.md
    audio/
      mic.m4a
      call.m4a
      mic-001.m4a
      call-001.m4a
      mic-002.m4a   # present after a continued take
      call-002.m4a
    .recall/
      metadata.json
      markers.md
      notes.md
      transcription/
        combined-timeline.md
        raw-tracks.md
        full-debug-transcript.md
      analysis/
        prompt.md
        agent-raw-output.json or agent-raw-output.jsonl
        agent-result.json
```

`meeting.md` is the normal reading view. `transcript.md` is the clean source record. Audio and internal/debug artifacts remain available without crowding the session root. Recall continues to recognize sessions created with the older expanded layout.

`sessions/` is ignored by Git because it contains private meeting data.

## Useful Commands

```sh
recall
recall --title "Project sync"
recall list
recall show latest
recall open latest
recall open latest --dir
recall export latest
recall sources
recall audio-tap-probe
recall transcribe latest
recall transcribe latest --engine whisper
recall transcribe latest --engine parakeet
recall transcribe latest --track call
recall transcribe latest --track mic
recall analyze latest --agent grok
recall analyze latest --agent opencode
recall agents list
recall agents doctor
recall update
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
