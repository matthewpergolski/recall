# Transcription

Recall's transcription path is free-first and local-first.

Current command:

```sh
recall transcribe latest
recall transcribe latest --track call
recall transcribe latest --track mic
recall transcribe /path/to/session --track both
```

The command transcribes existing session audio and writes:

```text
sessions/<session-id>/transcript.md
```

The transcript includes:

- a combined timestamp-sorted timeline across tracks
- the call-audio transcript section
- the microphone transcript section

The combined timeline is not full speaker diarization. It interleaves `call` and `mic` segments by timestamp, which is better for conversation flow, but duplicate text can still appear when the microphone also hears meeting audio from speakers/headphones.

## Known Transcript Quality Issues

### Mic bleed from speakers

If meeting audio plays through speakers while Recall records the microphone, the mic track can capture the remote speaker acoustically. That means the same remote speech may appear in both:

- `audio/call.m4a`, as direct system/call audio
- `audio/mic.m4a`, as speaker bleed picked up by the microphone

This is expected with open speakers and an unisolated microphone. It is not necessarily a capture bug.

Best current mitigation:

- Use headphones or earbuds during calls so remote audio stays mostly out of the mic track.

Future software mitigations:

- Add overlap dedupe in the combined timeline. If `call` and `mic` segments overlap and contain very similar text, prefer the cleaner source and suppress the duplicate.
- Add a "prefer call on overlap" merge mode for remote-speaker-heavy meetings.
- Investigate macOS voice-processing / echo-cancellation input for microphone capture.
- Add optional speaker labeling after transcript quality is stable.

Status: not implemented yet. The current combined timeline is chronological only.

## Current Behavior

`recall transcribe` is separate from the TUI for now. That is intentional while the local transcription dependency and model setup are being validated.

End-state behavior should be:

1. User ends a recording in the TUI.
2. Recall finalizes `audio/mic.m4a` and `audio/call.m4a`.
3. Recall automatically starts transcription.
4. Recall writes `transcript.md`.
5. Recall later writes `summary.md` and `actions.md` from the transcript.

## Required Local Tools

Recall currently expects:

- `ffmpeg`
- `whisper-cli` from `whisper.cpp`
- a local ggml Whisper model file

See `docs/DEPENDENCIES.md` for the full dependency map, including no-Brew corporate setup.

`ffmpeg` must be available on `PATH` for the current implementation. This is a temporary dependency; the intended no-Brew direction is to replace it with Swift/AVFoundation audio conversion.

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

The first transcription pass writes separate sections for call audio and microphone audio. That avoids pretending we have diarization before we actually do.

Later work can merge the tracks by timestamp and add speaker labels.
