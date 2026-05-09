# Recall Roadmap

This document explains the shape of the product across milestones. It is intentionally plain-English so the project stays easy to reason about while the implementation grows.

## What Exists Right Now

The current codebase is a foundation recorder.

Implemented:

- Rust CLI project
- Interactive terminal dashboard
- Keyboard controls
- TUI `--title` and `--consent provided` options
- `recall start` command
- `recall list` command
- `recall show latest` command
- `recall sources` command
- `recall audio-tap-probe` command
- `recall transcribe latest` command
- Local `sessions/` folder structure
- `recall.json` metadata
- `summary.md`, `actions.md`, and initial `transcript.md` files
- Basic tests for session naming and metadata helpers
- Swift helper source listing
- Swift helper microphone recording
- Swift helper ScreenCaptureKit fallback system-audio recording
- CoreAudio process-tap system/call audio recorder
- TUI starts both microphone and system-audio recorders
- readable local timestamp session IDs
- local Whisper transcription with combined timeline

Not implemented yet:

- Automatic transcription when a TUI session ends
- Transcript dedupe when mic and call tracks contain overlapping remote speech
- Real summaries/action extraction

## v0: Product Shell

Purpose: make Recall feel like a real app before solving the hardest audio problems.

v0 should include:

- Flashy terminal dashboard: done
- Keyboard controls: done
- Session lifecycle: ready, recording, paused, ended: done
- Consent state shown in the UI: done
- Source picker mock: done
- Source detection: done
- Local session folder creation: done
- Session metadata and placeholder notes: done

Success criteria:

- Running `recall` opens a proper TUI: done
- Pressing start creates a session: done
- Ending a session leaves a clean folder on disk: done
- The app feels like the shape of the final product, even with fake meters.

## v1: Local macOS Recorder

Purpose: prove the core promise on macOS.

v1 should include:

- Swift capture helper
- Microphone capture: done for TUI-driven default mic recording
- App/system audio capture using ScreenCaptureKit: fallback helper path added
- CoreAudio process-tap path: recorder added, smoke-tested, and verified on one real call
- Separate mic and call audio files where feasible: initial `mic.m4a` and `call.m4a` paths added
- Live level meters fed by real audio: mic done, call done through CoreAudio tap events
- macOS permission handling notes
- Session folder writes real audio into `audio/`
- Manual local transcription command: done

Success criteria:

- User can join a Teams/Zoom/Slack/FaceTime/browser call.
- User can start Recall.
- Recall captures local mic plus call audio locally.
- Audio files can be played back after the meeting.

Current progress: the TUI starts the Swift helper and writes default microphone audio to `audio/mic.m4a`. It also starts a CoreAudio process-tap system-audio recorder targeting `audio/call.m4a`. A real call test produced valid non-silent mic and call audio.

## v2: Transcript and Notes

Purpose: turn recordings into useful written memory.

v2 should include:

- Audio chunking
- Local transcription path: initial manual command done
- Optional cloud transcription path
- Timestamped transcript: combined timeline done
- Summary
- Decisions
- Action items
- Questions
- Follow-ups

Success criteria:

- After ending a session, Recall produces useful Markdown notes.
- Each action/decision can be traced back to transcript timestamps.
- Output is useful for normal personal, family, school, and work meetings.

Current gap: transcription is manual (`recall transcribe latest`) rather than automatic after session end, and combined timeline dedupe/speaker labeling still need work.

## v3: Recall Library

Purpose: make the app useful across many meetings.

v3 should include:

- Session search
- Browse previous sessions
- Tags
- Better titles
- Export to Markdown folders, Obsidian, Apple Notes, or email
- Config file for storage location and defaults

Success criteria:

- Recall becomes a local meeting memory archive, not just a recorder.

## v4: Context-Aware Recall

Purpose: add optional domain intelligence without making the app only for developers.

v4 should include:

- Optional project mode when launched inside a repo
- Optional code-action extraction
- Optional issue/PR checklist output
- Optional calendar context
- Optional contact/person context

Success criteria:

- General users get clean notes.
- Developers get repo-aware follow-up when they want it.

## End State

Recall should be a local-first meeting memory tool that runs from the terminal and feels polished enough to trust during real conversations.

The end-state app:

- Captures mic and call audio locally on macOS
- Shows a refined terminal dashboard
- Stores meeting data in understandable local folders
- Transcribes meetings
- Extracts decisions, action items, questions, risks, and follow-ups
- Lets users search and revisit prior meetings
- Supports consent-aware recording workflows
- Avoids stealth behavior
- Keeps cloud services optional

## Product Principles

- Local-first by default
- Consent-aware by design
- Useful to non-technical people
- Developer-aware only as an optional mode
- Plain files whenever possible
- Terminal-native, but visually polished
- Small native helpers for OS-specific capture
