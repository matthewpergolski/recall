# Audio Capture Notes

Recall has a preferred CoreAudio system/call-audio path and a ScreenCaptureKit fallback.

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

This path has produced audible system audio in controlled macOS-sound tests and real calls. The launching application's macOS permission remains part of the capture boundary.

## Microphone Device Changes

Recall records the default macOS microphone into `audio/mic.m4a`. This can be fragile if the user changes audio routes during a call, for example:

- starts on MacBook speaker/mic
- switches to AirPods
- AirPods disconnect or change Bluetooth profile
- macOS falls back to the MacBook microphone

The capture helper now reports the default input device at mic start and emits `device_changed` events when macOS changes the default input while recording. The TUI surfaces those changes in the capture-health line and live notes.

Current behavior is detection and warning. Starting already on AirPods records the mic for the full session. Switching to AirPods after recording has started is reported in the TUI; `mic.m4a` sometimes continues and sometimes ends at the switch. Recall does not yet restart capture onto the new input or stitch mic segments.

Future direction:

1. Lock recording to an explicit selected microphone.
2. Restart mic capture automatically when the default input changes.
3. Write segmented mic files, for example `mic-000.m4a`, `mic-001.m4a`.
4. Stitch or timeline-align mic segments during transcription.

## Launcher Permission Model

macOS attributes microphone and system-audio permission to the application that launches Recall. The `recall` executable does not currently appear as an independently signed macOS application.

This means:

- Apple Terminal permission applies only when Recall is launched from Terminal.
- Ghostty needs its own permission when Recall is launched from Ghostty.
- VS Code's integrated terminal attributes access to `Visual Studio Code.app`.
- Codex-launched diagnostics use Codex's permission.

For the preferred CoreAudio path, enable the actual launcher under **System Audio Recording Only** in **System Settings -> Privacy & Security -> Screen & System Audio Recording**. Enable that launcher separately under **Microphone**, then fully quit and reopen it.

Do not assume a successful tap probe or a finalized `call.m4a` proves capture quality. A permission problem can leave a full-duration file containing digital silence. Play known system audio and confirm that the TUI **Call** meter moves. If it remains at zero, stop the test and fix the launcher's permission before recording a real meeting.

For routine use, prefer one dedicated terminal application so permissions remain narrowly scoped and predictable. Avoid granting broad screen capture to an editor when the audio-only permission is sufficient.
