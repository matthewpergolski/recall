# Audio Capture Notes

Recall currently has two macOS system/call-audio capture paths under investigation.

## ScreenCaptureKit Path

Current command:

```sh
recall-capture record-system --session-dir ../sessions/example --duration 5
```

This path uses ScreenCaptureKit `SCStream` with `capturesAudio = true`. It can produce `audio/call.m4a`, but it triggers macOS's broad Screen & System Audio Recording permission and may show the private-window-picker bypass prompt.

Apple docs:

- [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
- [SCStreamConfiguration.capturesAudio](https://developer.apple.com/documentation/ScreenCaptureKit/SCStreamConfiguration/capturesAudio)

## CoreAudio Process-Tap Path

Current recorder command:

```sh
swift run recall-capture record-audio-tap --session-dir ../sessions/example --duration 5
```

Current probe command:

```sh
cargo run -- audio-tap-probe
```

or:

```sh
cd capture-helper
swift run recall-capture probe-audio-tap
```

The recorder creates a private CoreAudio process tap, wraps it in a private aggregate device, reads the aggregate input with an IO callback, and writes `audio/call.m4a`. A short audible system-sound smoke test produced level events and a valid M4A file.

The probe creates and destroys a private CoreAudio process tap without writing audio. On the current machine, the probe succeeds:

```json
{"message":"CoreAudio process tap created with id 166","source":"call","type":"audio_tap_probe_ok"}
{"message":"CoreAudio process tap destroyed","source":"call","type":"audio_tap_probe_stopped"}
```

This is the preferred implementation direction because it appears aligned with system-audio-only capture rather than screen-content capture.

Relevant local SDK APIs:

- `AudioHardwareCreateProcessTap`
- `AudioHardwareDestroyProcessTap`
- `CATapDescription`
- `kAudioAggregateDeviceTapListKey`

Current implementation shape:

1. Build a private aggregate device around the process tap.
2. Read the aggregate/tap audio through CoreAudio.
3. Write `audio/call.m4a`.
4. Prefer this path over ScreenCaptureKit for Recall's default system-audio capture.

Next engineering step: verify this path on a real Teams/Zoom/Slack/browser call, not only macOS system sounds.

## Microphone Device Changes

Recall records the default macOS microphone into `audio/mic.m4a`. This can be fragile if the user changes audio routes during a call, for example:

- starts on MacBook speaker/mic
- switches to AirPods
- AirPods disconnect or change Bluetooth profile
- macOS falls back to the MacBook microphone

The capture helper now reports the default input device at mic start and emits `device_changed` events when macOS changes the default input while recording. The TUI surfaces those changes in the capture-health line and live notes.

Current behavior is detection and warning. If macOS or `AVAudioRecorder` stops the mic recorder during a route switch, the TUI now reports that the mic recorder stopped unexpectedly instead of silently continuing.

Future direction:

1. Lock recording to an explicit selected microphone.
2. Restart mic capture automatically when the default input changes.
3. Write segmented mic files, for example `mic-000.m4a`, `mic-001.m4a`.
4. Stitch or timeline-align mic segments during transcription.

## Dev Permission Guidance

When testing ScreenCaptureKit, macOS attributes permission to the app that launches Recall. If Recall is launched from VS Code's terminal, permission is granted to `Visual Studio Code.app`.

For routine testing, prefer launching Recall from a dedicated terminal app. Avoid granting broad screen/audio capture permission to VS Code long-term.
