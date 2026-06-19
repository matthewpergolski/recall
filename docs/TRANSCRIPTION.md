# Transcription

Recall's transcription path is free-first and local-first.

Current command:

```sh
recall transcribe latest
recall transcribe latest --track call
recall transcribe latest --track mic
recall transcribe /path/to/session --track both
recall transcribe latest --chunk-seconds 600
```

The command transcribes existing session audio and writes:

```text
sessions/<session-id>/transcript.md
```

`transcript.md` is the clean, summary-ready artifact. An AI summarizer or action-item extractor should use this file by default.

Debug and audit artifacts are written separately:

```text
sessions/<session-id>/transcription-debug/
  combined-timeline.md
  raw-tracks.md
  full-debug-transcript.md
```

The debug files include:

- the raw timestamp-sorted combined timeline across tracks
- the call-audio transcript section
- the microphone transcript section
- a full debug transcript containing clean, combined, and raw track sections

The clean conversation timeline is not full speaker diarization. It starts from the combined timestamped segments, suppresses likely duplicate mic segments, and trims obvious call-audio phrases from mixed mic segments. The raw combined timeline is kept in `transcription-debug/` for audit/debugging.

## Multi-Hour Calls

Recall chunks each audio track before sending it to `whisper-cli`. The default chunk size is 600 seconds, or 10 minutes.

That means a 2-hour call with both `call.m4a` and `mic.m4a` becomes roughly:

- 12 call-audio transcription chunks
- 12 microphone transcription chunks
- one final `transcript.md` with timestamps offset back into the full meeting

This avoids one huge intermediate WAV and gives visible progress:

```text
Transcribing call chunk 1/12...
Transcribing call chunk 2/12...
Transcribing mic chunk 1/12...
```

You can tune the chunk size:

```sh
recall transcribe latest --chunk-seconds 300
recall transcribe latest --chunk-seconds 900
```

Smaller chunks show progress more often and reduce per-process working size. Larger chunks reduce process startup overhead. The default 10-minute chunk is the current balanced choice.

Transcription time still scales with meeting length, number of tracks, model size, and machine speed. On Apple Silicon with `whisper.cpp`, short tests are fast, but multi-hour meetings should be expected to run after the call. The TUI starts this automatically in the background after the user ends the recording.

If you only need one side for a quick check, transcribe one track:

```sh
recall transcribe latest --track call
recall transcribe latest --track mic
```

## Current Goal: Clean Merged Transcript

The next product goal is to turn the current chronological combined timeline into a clean conversation transcript.

Success criteria:

- Keep one readable timeline across `call` and `mic` tracks.
- Preserve mic-only segments, because those usually represent the local speaker.
- Prefer `call` segments when the same remote speech appears in both tracks at nearly the same time.
- Suppress or mark duplicated mic segments caused by speaker bleed.
- Avoid deleting uncertain segments aggressively.
- Produce transcript text that is reliable enough to feed into summary, decision, and action-item extraction.

Do not treat summary/action extraction as the next milestone until this transcript merge quality is acceptable.

## Known Transcript Quality Issues

### Mic bleed from speakers

If meeting audio plays through speakers while Recall records the microphone, the mic track can capture the remote speaker acoustically. That means the same remote speech may appear in both:

- `audio/call.m4a`, as direct system/call audio
- `audio/mic.m4a`, as speaker bleed picked up by the microphone

This is expected with open speakers and an unisolated microphone. It is not necessarily a capture bug.

Best current mitigation:

- Use headphones or earbuds during calls so remote audio stays mostly out of the mic track.

Current software mitigation:

- The `Clean Conversation` section prefers call/system audio for likely duplicated remote speech.
- It suppresses mic segments that are mostly contained in overlapping call audio.
- It removes contiguous call-audio phrases from mixed mic segments when enough local mic text remains.

Future software mitigations:

- Tune the dedupe thresholds against more speaker-mode recordings.
- Add a stricter "prefer call on overlap" merge mode for remote-speaker-heavy meetings.
- Investigate macOS voice-processing / echo-cancellation input for microphone capture.
- Add optional speaker labeling after transcript quality is stable.

Status: initial implementation exists. It is conservative and should be validated against more real speaker-mode recordings before treating it as done.

### Model quality

The default documented model, `ggml-base.en.bin`, is fast and convenient, but real casual calls expose its limits. Proper nouns, local place names, fast speech, road noise, speakerphone bleed, and navigation prompts can produce odd words or repeated hallucinated phrases.

Before over-tuning merge heuristics, also test a larger local model:

```sh
recall transcribe latest --model models/ggml-small.en.bin
recall transcribe latest --model models/ggml-medium.en.bin
```

Larger models cost more local compute time but should improve transcript quality.

## Current Behavior

The TUI starts transcription automatically after the user presses Space or Enter to end a recording. The status area shows current transcript progress, including the active track/chunk, and shows the final `transcript.md` path when complete.

Recall can keep processing a finished session while the user starts another recording. Background transcription and analysis jobs are session-scoped, so a previous session finishing should not overwrite the currently active session display.

The direct command still exists for re-running or debugging transcription:

```sh
recall transcribe latest
recall transcribe /path/to/session
```

End-state behavior should be:

1. User ends a recording in the TUI.
2. Recall finalizes `audio/mic.m4a` and `audio/call.m4a`.
3. Recall automatically starts transcription.
4. Recall writes `transcript.md`.
5. If auto-analysis is enabled, Recall runs the selected headless agent and writes summary/action files from the transcript.

## Agent Analysis

Recall can pass the clean `transcript.md` to a headless CLI agent and write structured meeting memory files.

Manual analysis:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
recall analyze /path/to/session --agent codex
```

Dry run:

```sh
recall analyze latest --agent grok --dry-run
```

Supported built-in agent profiles:

```sh
recall agents list
recall agents doctor
```

Automatic TUI analysis:

```sh
recall --agent grok --auto-analyze
```

Or with a personal alias:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

Config defaults:

```toml
# ~/.config/recall/config.toml
consent_default = "provided"
storage_dir = "~/Documents/Recall/sessions"

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"

[transcription]
ffmpeg_bin = "~/Documents/Recall/tools/ffmpeg/bin/ffmpeg"
whisper_bin = "~/Documents/Recall/tools/whisper/bin/whisper-cli"
model_path = "~/Documents/Recall/models/ggml-base.en.bin"
chunk_seconds = 600
```

Analysis outputs:

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

Recall keeps control of file layout. The agent is asked to return one JSON object, including a concise title when possible. Recall uses that title to rename generic session folders, update metadata/headings, and render Markdown files from the normalized result.

## Required Local Tools

Recall currently expects:

- `ffmpeg`
- `whisper-cli` from `whisper.cpp`
- a local ggml Whisper model file

See `docs/DEPENDENCIES.md` for the full dependency map, including no-Brew corporate setup.

Recall finds `ffmpeg` in this order:

1. `--ffmpeg <path>`
2. `RECALL_FFMPEG_BIN`
3. `[transcription].ffmpeg_bin` in `~/.config/recall/config.toml`
4. `tools/ffmpeg/bin/ffmpeg`
5. `ffmpeg` on `PATH`

`ffmpeg` is still a temporary dependency; the intended no-Brew direction is to replace it with Swift/AVFoundation audio conversion.

If `whisper-cli` is not on `PATH`, set:

```sh
export RECALL_WHISPER_BIN=/path/to/whisper-cli
```

Fast personal setup with Homebrew:

```sh
brew install whisper-cpp
mkdir -p models
curl -L -o models/ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

No-Brew setup:

```text
tools/
  whisper/
    bin/
      whisper-cli
models/
  ggml-base.en.bin
```

Then point Recall at those files:

```sh
export RECALL_FFMPEG_BIN="$PWD/tools/ffmpeg/bin/ffmpeg"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
recall transcribe latest
```

If the model is not at `models/ggml-base.en.bin`, set:

```sh
export RECALL_WHISPER_MODEL=/path/to/ggml-model.bin
```

## Recommended Model Location

Keep models out of session folders:

```text
models/
  ggml-base.en.bin
```

Models can be large, so this folder should eventually be ignored if the project becomes a git repo.

## Track Strategy

Recall records two files:

- `audio/call.m4a`: meeting/app/system audio
- `audio/mic.m4a`: local microphone

Recall transcribes both tracks separately, then writes a clean merged `transcript.md` for normal use. The separate per-track transcripts are kept in `transcription-debug/raw-tracks.md`.

This preserves source separation while still giving future summarization code one obvious input file. Later work can add stronger speaker labels.
