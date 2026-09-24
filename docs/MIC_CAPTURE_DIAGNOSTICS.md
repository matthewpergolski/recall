# Connected-call microphone diagnostics

This is an opt-in diagnostic procedure, not a workaround or a claim that
Phone/FaceTime blocks microphone access. No audio routing, input volume, mute,
or system settings are changed. BlackHole is neither required nor configured.

## Build and run from this checkout

```sh
swift build --package-path capture-helper
python3 scripts/diagnose-call-mic.py
```

Use `--helper /absolute/path/to/recall-capture` if the tested helper is built
elsewhere. The runner does not use the installed Recall command. Python 3 is
required for this developer-only runner, not for normal capture.

If Xcode's build system encounters the test-bundle filesystem metadata signing
error seen during development, the verified temporary-build alternative is:

```sh
swift test --package-path capture-helper --scratch-path /tmp/recall-mic-diagnostic-build --build-system native -Xswiftc -parse-as-library
python3 scripts/diagnose-call-mic.py --helper /tmp/recall-mic-diagnostic-build/debug/recall-capture
```

The native build system is deprecated by this Xcode release; it is a temporary
test workaround, not a required change to the normal product build.

The runner asks before each phase. Each phase lasts 15 seconds by default:

1. Before connection: microphone only.
2. Connected call: microphone only.
3. Same connected call: microphone plus existing Core Audio system tap.
4. Same connected call: microphone only again.
5. After hangup: microphone only.

Place the call through Phone/FaceTime on the Mac, with a remote endpoint outside
the room. Verify the input selected in the call app as well as macOS's default
input. Record that information when prompted; the script cannot establish the
call app's selected microphone. Keep hardware, speaking position, gain, and mic
mode unchanged between phases. Repeat a distinct phrase directly toward the
selected microphone, including a phrase while the remote endpoint is silent.
If possible, use wired headphones without changing the microphone to avoid
speaker bleed. Test AirPods/route switching separately after this baseline.

Use only non-sensitive test phrases. Obtain consent when another person is on
the call. No recording occurs while the script waits at a prompt. Ctrl+C stops
children; forced interruption can leave an unfinished audio file. Permission
denial or a helper error stops the test rather than changing permissions.

## Results

For startup failures, `recall-capture diagnose-input` reads device controls and
device-level plus per-stream physical/virtual formats without starting audio IO.
It emits JSON on stderr. With diagnostics enabled, recording startup also logs
the device snapshot before format conversion, and logs a rejected ASBD in full.

The runner prints a unique system-temporary directory. It retains private audio,
JSON events, stderr diagnostic logs, and the manually supplied route description
there for comparison. Nothing is sent to a transcription model or analysis agent.
Delete this directory after diagnosis; do not commit or publicly upload it.

`RECALL_MIC_DIAGNOSTICS=1` enables the helper diagnostics. Recall does not turn this on by itself. A normal `recall` launch does not scan
raw buffers for these statistics or poll these extra device properties.

- `mic_pcm_diagnostic`: roughly one-second windows of raw HAL RMS/peak, exact-zero
  and nonfinite sample counts, copied PCM RMS/frame counts, callback gaps,
  Recall mute state, sampled writer delay, and cumulative file frame count.
- `mic_device_diagnostic`: captured device ID/name/UID, system default input/output
  IDs, hog PID, input volume/mute on master and channels 1/2, data source and format.
  Unsupported controls retain their OSStatus rather than being reported as zero.

Raw statistics are measured from the supplied AudioBufferList before frame
clamping/copying, Recall mute, encoding, or live transcription. Copied statistics
measure the PCM submitted to the writer, also before mute. Float32 PCM is measured
without the TUI meter's -60 dB floor. Other raw formats are explicitly unsupported
by this diagnostic scan. Null raw RMS means no finite nonzero energy; inspect the
sample/missing/unsupported counts to distinguish silence from missing evidence.
The final sub-second window may not be reported. Writer delay is sampled at
window boundaries, not a maximum across all callbacks. Property snapshots are
polled once a second and can miss brief transitions.

Decode the saved M4A and compare RMS/duration with those windows, allowing for AAC
priming and lossy encoding. Do not judge success solely by a transcript:

- Raw healthy, copied quiet: investigate PCM conversion/frame handling.
- Raw/copied healthy, file quiet or short: investigate writer/encoding/queue.
- Mic-only healthy, combined quiet: investigate interaction with system capture.
- Both connected phases quiet at raw input: investigate actual call routing,
  hardware controls, format changes, mic modes, or OS behavior. This alone does
  not prove an OS prohibition or justify another API change.

System audio is process playback, not a guaranteed two-sided call mix. A voice
on `call.m4a` is not evidence that the local microphone path is working.

## Three-channel format correction

A connected Phone call on the tested Mac changes its built-in microphone from
48 kHz mono (4 bytes/frame) to 48 kHz, three interleaved Float32 channels
(12 bytes/frame), without changing its device ID. This is an observed case, not
a claim about every Mac or every FaceTime call.

The old AVAudioFormat initializer rejects more than two channels without a
channel layout. Mic capture now supplies a discrete layout and copies callbacks
using the actual AudioBufferList channel packing, rather than treating a new
three-channel callback as cached mono. It also checks same-device format changes
and reopens the input. Brief stop/start gaps remain possible and must be checked
on a real call; synthetic format tests cannot prove uninterrupted hardware IO.

Multichannel mic parts use Apple Lossless in M4A because discrete three-channel
AAC failed in local encoder tests. All original channels remain in those parts;
they can be larger than ordinary mono AAC. Live speech gets a normalized
channel-index mono mix. Final transcription also mixes channel indices explicitly
so a mic channel is not discarded as a presumed surround/LFE channel. Mixing is
not beamforming or echo cancellation; real-call intelligibility remains a gate.

Different-format parts are decoded independently to a common mono PCM format
before joining for transcription. Original parts remain untouched. This uses
temporary disk space and reencoding time; failed/finished joins clean that scratch
directory. The temporary normalized files are not a substitute for raw channels.

Reference: [Apple AVAudioFormat initializer documentation](https://developer.apple.com/documentation/avfaudio/avaudioformat/init(streamdescription:)).
