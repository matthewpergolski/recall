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
- Chunked transcription for long recordings.
- Combined transcript timeline across call and mic tracks.
- Initial clean conversation timeline with conservative mic-bleed dedupe.
- Summary-ready `transcript.md` now contains only the clean transcript.
- Raw combined/per-track transcript material moved to `transcription-debug/`.
- Automatic TUI transcription after ending a session, with progress/status display.
- Headless agent analysis command: `recall analyze latest --agent <name>`.
- Mic input device-change detection and TUI warnings for early mic recorder exits.
- Built-in agent profiles for Grok, Cline, Codex, and Claude.
- Optional TUI auto-analysis via `--agent <name>` or configured default agent.
- Config defaults via `~/.config/recall/config.toml`.
- TUI session title option and readable Eastern Time session IDs.
- Agent-generated session titles replace generic "Quick Capture" headings and folder slugs after analysis.
- Memory bank and repo agent instructions are in place.
- Portability docs and first-session testing docs are in place.

## Current Focus

- Validate and tune the clean merged transcript against more real speaker-mode calls.
- Reduce remaining duplicate mic/call overlap when the microphone hears remote audio from speakers.
- Use the clean transcript as the input for later summaries, decisions, and action items.

## Next

- Test the clean transcript on another speaker-mode call.
- Tune overlap dedupe thresholds and phrase trimming based on real output.
- Consider a stricter "prefer call on overlap" transcript merge mode.
- Investigate macOS voice-processing / echo-cancellation input for microphone capture.
- Add automatic mic recorder restart and segmented mic stitching after route changes.
- Persist TUI markers and notes into session files.
- Validate real agent outputs from Grok/Cline/Claude/Codex and tune JSON extraction as needed.
- Improve generated summary/action prompts after transcript quality is reliable.

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
