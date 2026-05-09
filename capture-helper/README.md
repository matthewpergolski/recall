# macOS Capture Helper

This folder contains the Swift helper used by the Rust TUI.

## Why Swift

Rust is a good fit for the terminal app, session state, local storage, and packaging. macOS audio capture is best implemented with Apple-native frameworks.

The helper should stay small:

- list capture-capable apps/windows
- capture microphone audio
- capture app/system audio with ScreenCaptureKit
- write audio chunks or stream PCM events
- emit simple JSON status events to the Rust app

## Planned APIs

Current command shape:

```sh
swift run recall-capture list-sources
swift run recall-capture record-mic --session-dir ../sessions/example --duration 5
swift run recall-capture record-audio-tap --session-dir ../sessions/example --duration 5
swift run recall-capture record-system --session-dir ../sessions/example --duration 5
swift run recall-capture probe-audio-tap
```

Planned command shape:

```sh
recall-capture list-sources
recall-capture record --session-dir ./sessions/example --app "Microsoft Teams" --mic default
```

Current `list-sources` output shape:

```json
{
  "type": "source_list",
  "version": "0.1.0",
  "candidates": [],
  "microphones": [],
  "permissions": {
    "microphone": "not_determined"
  }
}
```

Current `record-mic` event output is newline-delimited JSON:

```json
{"elapsedSeconds":0,"path":"../sessions/example/audio/mic.m4a","source":"mic","type":"recording_started"}
{"elapsedSeconds":0.25,"levelDb":-48.2,"source":"mic","type":"level"}
{"elapsedSeconds":5.01,"path":"../sessions/example/audio/mic.m4a","source":"mic","type":"recording_stopped"}
```

Current `record-system` event output uses the same newline-delimited shape with `source` set to `call` and writes `audio/call.m4a` when ScreenCaptureKit capture is permitted.

Current `record-audio-tap` event output uses the same newline-delimited shape with `source` set to `call`, writes `audio/call.m4a`, and emits level events from CoreAudio process-tap buffers.

Current `probe-audio-tap` event output creates and destroys a private CoreAudio process tap:

```json
{"message":"CoreAudio process tap created with id 166","source":"call","type":"audio_tap_probe_ok"}
{"message":"CoreAudio process tap destroyed","source":"call","type":"audio_tap_probe_stopped"}
```

Potential future event output:

```json
{"type":"source_detected","kind":"app","name":"Microsoft Teams"}
{"type":"level","source":"mic","db":-18.2}
{"type":"level","source":"call","db":-12.4}
{"type":"recording_started"}
{"type":"recording_stopped"}
```

## macOS Frameworks

- ScreenCaptureKit
- AVFoundation
- CoreAudio, if needed

## Current Boundary

The helper currently lists candidate sources, records default microphone audio, records system audio through CoreAudio process taps, keeps an initial ScreenCaptureKit fallback command, and can probe CoreAudio process taps. The Rust TUI invokes the mic and CoreAudio process-tap recorders.
