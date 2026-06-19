# Recall v0 Spec

## Goal

Build a macOS terminal app that captures meeting audio locally and produces useful meeting notes.

Recall should feel polished and calm, with a Claude Code-style terminal interface. It should be useful for general meetings, not only software engineering meetings.

## Primary User Flow

1. User joins a call in Teams, Zoom, Slack, FaceTime, or a browser.
2. User gets consent to record locally for note-taking.
3. User runs `recall`.
4. Recall detects likely call sources and microphone input.
5. User confirms sources and marks consent as obtained.
6. Recall records microphone and call audio locally.
7. User ends the session.
8. Recall saves audio, metadata, transcript-ready files, and notes.

## v0 Features

- Interactive terminal dashboard
- TUI startup flag: `--consent provided`
- Start session command
- List sessions command
- Show latest session command
- Visible recording state
- Consent metadata field
- Source picker shell
- Local session folder creation
- Session metadata JSON
- Markdown note output
- Real audio level meters when capture is active
- macOS capture helper

## v1 Features

- Real microphone capture: TUI-driven default mic recording exists
- Real app/system audio capture with CoreAudio process taps: initial helper/TUI path exists
- ScreenCaptureKit fallback path remains available
- Audio level meters: mic real, call real through CoreAudio tap events
- Separate local/user track and call/system track where possible: `mic.m4a` and `call.m4a`
- Manual transcription pipeline: `recall transcribe latest`
- Combined timestamped transcript timeline
- Summary generation
- Decisions
- Action items
- Questions and follow-ups

## Later Features

- Live transcription
- Speaker diarization
- Search across meetings
- Obsidian/Apple Notes export
- Calendar integration
- Optional project/repo-aware action extraction
- Linux and Windows capture backends

## Explicit Non-Goals

- Stealth recording
- Hiding recording state
- Bypassing macOS permission prompts or indicators
- Cloud sync by default
- Windows/Linux support in the initial macOS implementation

## Session Folder Shape

```text
sessions/
  05-26-2026_7-21pm-et-design-sync/
    recall.json
    audio/
      mic.m4a
      call.m4a
    transcript.md
    summary.md
    actions.md
    decisions.md
    questions.md
    followups.md
    markers.md
    notes.md
    transcription-debug/
      combined-timeline.md
      raw-tracks.md
      full-debug-transcript.md
    analysis-debug/
      prompt.md
      agent-raw-output.json or agent-raw-output.jsonl
      agent-result.json
```

## Metadata Sketch

```json
{
  "id": "05-26-2026_7-21pm-et-design-sync",
  "title": "Design sync",
  "created_at_unix": 1778281380,
  "status": "initialized",
  "consent": { "mode": "verbal" },
  "sources": {
    "microphone": null,
    "call_audio": null
  }
}
```

`recall show latest`, `recall transcribe latest`, and `recall analyze latest` sort sessions by `created_at_unix` rather than folder name.
