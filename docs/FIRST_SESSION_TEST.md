# First Session Test

Use this runbook to verify that Recall captures both your microphone and call/system audio.

## Before You Start

Use a dedicated terminal app for routine testing, not the VS Code integrated terminal. This keeps macOS capture permissions scoped to the terminal rather than your editor.

Check the installed command:

```sh
recall --version
recall sources
recall audio-tap-probe
```

Expected:

- `recall sources` lists Teams/Zoom/browser apps and microphones.
- `recall audio-tap-probe` prints `audio_tap_probe_ok` and `audio_tap_probe_stopped`.

## Teams Test Call

Microsoft documents Teams test calls in the desktop app under:

```text
Settings and more -> Settings -> Devices -> Make a test call
```

Use the Teams desktop app, not Teams on the web. Microsoft notes that the test call feature is available in the Teams desktop app for Windows and Mac.

Test flow:

1. Open Microsoft Teams.
2. Go to Settings and more, then Settings, then Devices.
3. Choose your normal speaker and microphone.
4. Click Make a test call.
5. Start Recall from a terminal:

```sh
recall --consent provided
```

6. Press Enter in Recall to start recording.
7. In the Teams test call, say a short phrase out loud.
8. Let Teams play your phrase back.
9. Press `e` in Recall to end the session.
10. Press `q` to quit Recall.

The important part is that the test includes both:

- your live microphone voice, for `audio/mic.m4a`
- Teams playback/system audio, for `audio/call.m4a`

If you only talk into your own microphone and no app plays audio back, `call.m4a` may be silent. That does not prove the call capture is broken; it means there was no system/call audio to capture.

## Alternative Real Call Test

If Teams test call is not available, use a short call with a second device or another person.

1. Join a Teams, Zoom, Slack, FaceTime, or browser call.
2. Get consent to record locally for note-taking.
3. Start Recall:

```sh
recall --consent provided
```

4. Press Enter to start recording.
5. Have both sides say short test phrases:

```text
Local mic: "This is my microphone test."
Remote audio: "This is the call audio test."
```

6. Press `e` to end the session.
7. Press `q` to quit.

## Quick System-Audio Test

This is not a meeting test, but it confirms `audio/call.m4a` can capture sound played by macOS.

From the repo root:

```sh
mkdir -p sessions/manual-system-audio-test
cd capture-helper
(for i in {1..4}; do afplay /System/Library/Sounds/Ping.aiff; done) &
swift run recall-capture record-audio-tap --session-dir ../sessions/manual-system-audio-test --duration 3
```

Then play:

```sh
open ../sessions/manual-system-audio-test/audio/call.m4a
```

## Verify Output

Show the latest session:

```sh
recall show latest
```

Open the latest session folder in VS Code:

```sh
code "$(recall show latest)"
```

Expected files:

```text
audio/
  mic.m4a
  call.m4a
```

Reliable playback on macOS:

```sh
open "$(recall show latest)/audio/mic.m4a"
open "$(recall show latest)/audio/call.m4a"
```

VS Code may show the files in the explorer, but built-in playback depends on local VS Code/media support and extensions. The `open` command is the most reliable way to listen because it uses the default macOS audio player.

## Pass Criteria

The session passes if:

- `mic.m4a` exists and plays your local microphone.
- `call.m4a` exists and plays the Teams/Zoom/browser call audio.
- Recall does not need VS Code's broad Screen & System Audio Recording permission when launched from a dedicated terminal.

## Current Gaps

Recall can now run a separate local transcription command after the session:

```sh
recall transcribe latest
```

This requires `ffmpeg`, a local `whisper.cpp` CLI, and a local ggml Whisper model. `summary.md` and `actions.md` remain placeholders until transcript-based analysis is added.

Source: [Microsoft Teams test call documentation](https://support.microsoft.com/en-gb/office/manage-your-call-settings-in-microsoft-teams-456cb611-3477-496f-b31a-6ab752a7595f).
