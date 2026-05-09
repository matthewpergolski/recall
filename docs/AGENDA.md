# Agenda

## Checked Off

- Product shell: Rust CLI and `ratatui` TUI.
- Consent-aware start flow with `--consent provided`.
- Local session folders under `sessions/`.
- Source detection for meeting apps and microphones.
- Microphone recording to `audio/mic.m4a`.
- CoreAudio process-tap probe.
- CoreAudio system/call audio recording to `audio/call.m4a`.
- TUI starts and stops mic plus system audio together.
- Installed `recall` binary refreshed after capture changes.
- First local smoke test writes both `mic.m4a` and `call.m4a`.
- Real call test writes valid non-silent `mic.m4a` and `call.m4a`.
- Local transcription command scaffold: `recall transcribe latest`.
- Transcription docs: `docs/TRANSCRIPTION.md`.
- Combined transcript timeline across call and mic tracks.
- TUI session title option and readable local timestamp session IDs.
- Memory bank and repo agent instructions are in place.
- Portability docs and first-session testing docs are in place.

## Current Focus

- Improve transcript quality by reducing duplicate mic/call overlap.
- Use the transcript as the input for later summaries, decisions, and action items.

## Next

- Add overlap dedupe for cases where the mic captures the remote speaker through speakers.
- Consider a "prefer call on overlap" transcript merge mode.
- Investigate macOS voice-processing / echo-cancellation input for microphone capture.
- Add optional automatic transcription after ending a TUI session.
- Persist TUI markers and notes into session files.
- Add summary, decisions, action items, questions, and follow-ups after transcription is reliable.

## Transcription Plan

Prioritize transcript generation before higher-level analysis.

The free-first path should be:

1. Convert or feed captured audio into a local transcription backend.
2. Write timestamped text into `transcript.md`.
3. Preserve links from transcript timestamps back to source audio.
4. Generate `summary.md` and `actions.md` from the transcript.

Likely local transcription options:

- `whisper.cpp`: strong free-first candidate; local model files, no cloud dependency.
- `faster-whisper`: Python-based option; likely more setup and larger runtime dependencies.
- Apple Speech APIs: local-ish system integration, but behavior and permissions need separate evaluation.

Analysis should come after transcript quality is acceptable. Action extraction without a reliable transcript will be brittle.
