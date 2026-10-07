# Transcription

Recall's transcription path is free-first and local-first.

Current command:

```sh
recall transcribe latest
recall transcribe design-sync
recall transcribe latest --track call
recall transcribe latest --track mic
recall transcribe /path/to/session --track both
recall transcribe latest --chunk-seconds 600
recall transcribe latest --engine apple
recall transcribe latest --engine whisper
recall transcribe latest --engine parakeet
```

The command transcribes existing session audio and writes:

```text
sessions/<session-id>/transcript.md
```

`transcript.md` is the clean, summary-ready artifact. An AI summarizer or action-item extractor should use this file by default.

Debug and audit artifacts are written separately:

```text
sessions/<session-id>/.recall/transcription/
  combined-timeline.md
  raw-tracks.md
  full-debug-transcript.md
```

The debug files include:

- the raw timestamp-sorted combined timeline across tracks
- the call-audio transcript section
- the microphone transcript section
- a full debug transcript containing clean, combined, and raw track sections

The clean conversation timeline is not full speaker diarization. It starts from the combined timestamped segments, suppresses likely duplicate mic segments, and trims obvious call-audio phrases from mixed mic segments. The raw combined timeline is kept in `.recall/transcription/` for audit/debugging. Bleed is decided from the audio where the two tracks share a timeline, and from the words otherwise; see "Mic bleed from speakers" below.

## One Timeline for Both Tracks

The microphone and the call audio are recorded by two recorders that start a moment apart, and a track can roll into a new part when the microphone changes format in a call. While recording, each recorder writes the start time of each audio part, on the Mac's own clock, to `.recall/timeline/<file>.json`. The record holds times only, no audio.

At transcription, Recall places each part at its start time:

- Each track is shifted so that both count from the take's first sample.
- Time lost between two parts of a track is kept as silence in the work copy. The files in `audio/` are not changed.
- A session with several takes gives each take its own base; takes follow one another.

A time in `transcript.md` then means the same moment on both tracks, so lines are in true order and the bleed rules compare the right lines.

Recall joins the parts back to back, as older versions did, when the start times cannot be used: a session recorded before this existed, the ScreenCaptureKit fallback recorder, a part with no record, or a start time that cannot be right. `.recall/transcription/full-debug-transcript.md` has a `## Timeline` paragraph that says which happened, how much silence was kept, and how many mic lines the bleed rules suppressed with and without the placement.

## Multi-Hour Calls

Recall chunks each audio track with `ffmpeg` before sending it to the selected ASR engine. The default engine on Apple Silicon macOS 26+ is Apple SpeechAnalyzer. `--engine parakeet` uses `parakeet-mlx`. `--engine whisper` uses `whisper-cli`. The default chunk size is 600 seconds, or 10 minutes. If Apple is the default and SpeechAnalyzer is unavailable, Recall falls back to Parakeet. If SpeechAnalyzer passes its check and then fails on the audio, Recall runs the transcription again on the fallback. The fallback note gives the reason in both cases. Passing `--engine apple` or config `engine = "apple"` errors instead of falling back.

That means a 2-hour call with both `call.m4a` and `mic.m4a` becomes roughly:

- 12 call-audio transcription chunks
- 12 microphone transcription chunks
- one final `transcript.md` with timestamps offset back into the full meeting

This avoids one huge intermediate WAV and gives visible progress:

```text
Transcribing call chunk 1/12...
Transcribing call chunk 2/12...
Transcribing mic chunk 1/12...
```

You can tune the chunk size:

```sh
recall transcribe latest --chunk-seconds 300
recall transcribe latest --chunk-seconds 900
```

Smaller chunks show progress more often and reduce per-process working size. Larger chunks reduce process startup overhead. The default 10-minute chunk is the current balanced choice.

Transcription time still scales with meeting length, number of tracks, model size, and machine speed. On Apple Silicon with `whisper.cpp`, short tests are fast, but multi-hour meetings should be expected to run after the call. The TUI starts this automatically in the background after the user ends the recording.

The first Parakeet run downloads NVIDIA Parakeet TDT 0.6B v3 (~1.2 GB) into the Hugging Face cache (`~/.cache/huggingface/hub/models--mlx-community--parakeet-tdt-0.6b-v3` unless `HF_HOME` or `--parakeet-cache-dir` is set). Recall treats that as its own phase: the TUI says it is downloading, shows MB downloaded / total, rate, and the cache path, and does not label it as transcription progress. Later sessions reuse the cache. If the download fails (offline, proxy, stall), the transcript status is a download error rather than a frozen 8%.

If you only need one side for a quick check, transcribe one track:

```sh
recall transcribe latest --track call
recall transcribe latest --track mic
```

## Current Goal: Clean Merged Transcript

The next product goal is to turn the current chronological combined timeline into a clean conversation transcript.

Success criteria:

- Keep one readable timeline across `call` and `mic` tracks.
- Preserve mic-only segments, because those usually represent the local speaker.
- Prefer `call` segments when the same remote speech appears in both tracks at nearly the same time.
- Suppress or mark duplicated mic segments caused by speaker bleed.
- Avoid deleting uncertain segments aggressively.
- Produce transcript text that is reliable enough to feed into summary, decision, and action-item extraction.

Do not treat summary/action extraction as the next milestone until this transcript merge quality is acceptable.

## Known Transcript Quality Issues

### Mic bleed from speakers

If meeting audio plays through speakers while Recall records the microphone, the mic track can capture the remote speaker acoustically. That means the same remote speech may appear in both:

- `audio/call.m4a`, as direct system/call audio
- `audio/mic.m4a`, as speaker bleed picked up by the microphone

This is expected with open speakers and an unisolated microphone. It is not necessarily a capture bug.

Best current mitigation:

- Use headphones or earbuds during calls so remote audio stays mostly out of the mic track.

How Recall decides, for each line on the mic track:

Recall measures the loudness of both tracks in 10 ms steps and decides from the audio first. The recognizer garbles bleed and invents words from room noise, so the words alone cannot tell.

1. **No speech under the line.** The line is left out when the mic's audio never rises above the room: less than 15% of the line is 6 dB over the quiet level of its audio part, and the line's level is under 6 dB over it. A line with no words, such as a lone ".", is left out too. This rule needs only the mic track, so it applies to every session.
2. **The call side is silent during the line.** The line is kept, whatever its words. A real "yeah" or "right" stays.
3. **The line is only the call's sound.** Two measurements must agree. The mic's loudness follows the call's, at a match of 0.55 or more with the mic within 250 ms of where the timeline put it. And when the call's sound is cancelled out of the mic, 30% or less of the line is left. Then the line is dropped as bleed, however short or garbled. The second measurement is what protects a person who speaks over loud speakers: their voice is what cancelling leaves behind.
4. **The mic heard something else.** At a match under 0.30 the line is kept, even when its words are the call's.
5. **Otherwise the words decide,** as in older versions: a mic line whose words are mostly in the call lines beside it is dropped, and call phrases are trimmed out of a mixed line.

Rules 2 to 4 need both tracks on one timeline (see "One Timeline for Both Tracks"). Without it, and for a line the audio cannot speak for, only rule 5 applies. When the mic matches the call only in a wider search, the tracks were placed wrong, and the words decide for that line.

Cancelling uses ffmpeg's adaptive filter (`anlms`, in ffmpeg 5.1 and newer) as a measurement only. No audio file is changed. The filter needs a few seconds to settle when the far side starts, after the user speaks, and after a device change. Bleed in those seconds looks like the user's voice, so it goes to rule 5 and some of it stays. With an older ffmpeg, rule 3 never applies.

`transcript.md` counts the bleed that went: "Suppressed N likely duplicate mic segments caused by speaker bleed". The lines left out by rule 1 are listed under the conversation, each with its time and its words, because a very quiet voice measures the same as made-up words: speech about 14 dB under a normal voice was left out in a test. The summary agent is not sent that list; see `docs/AGENT_ANALYSIS.md`. Nothing is deleted from disk. `.recall/transcription/full-debug-transcript.md` has a `## Mic Lines and the Audio` section with every mic line, what was measured, and which rule decided, and the combined timeline there still holds every line.

Limits:

- The numbers come from three recordings with one far voice, on a Studio Display and a MacBook. Other rooms and speakers may need other numbers.
- Some bleed stays: short lines and garbled lines from the seconds when the filter has not settled.
- A voice much quieter than the bleed around it may leave too little behind to be noticed.
- Headphones remain the best fix: with no bleed there is nothing to decide.
- This removes lines from the transcript. It does not remove bleed from the audio, and it is not echo cancellation or speaker labeling.

### Model quality

The default documented model, `ggml-base.en.bin`, is fast and convenient, but real casual calls expose its limits. Proper nouns, local place names, fast speech, road noise, speakerphone bleed, and navigation prompts can produce odd words or repeated hallucinated phrases.

On silent or near-silent call audio, Whisper often invents short polite phrases such as `You` or `Thank you.` Those lines are model hallucinations, not meeting speech. A moving **Call** meter during capture is the check that the call track actually had sound. The current local default on this machine is `ggml-large-v3-turbo.bin`, which is stronger than `base.en` but can still hallucinate on empty audio.

Default Apple Silicon backend: on-device **Apple SpeechAnalyzer**. Parakeet (`parakeet-mlx`) and Whisper/`whisper-cli` remain first-class engines. They are not drop-in ggml files.

```sh
recall transcribe latest
recall transcribe latest --engine apple
recall transcribe latest --engine parakeet
recall transcribe latest --engine whisper
```

`--engine whisper` and `--engine parakeet` are always valid. Apple SpeechAnalyzer needs macOS 26+ and an on-device speech model already installed; Recall does not auto-download Apple speech assets or use cloud recognition. `recall doctor` lists the installed locales and says whether macOS reports a pending install request. A pending request for a locale that is already installed does not block Apple Speech. The Parakeet path needs `parakeet-mlx` on PATH (or `RECALL_PARAKEET_BIN`) and may download `mlx-community/parakeet-tdt-0.6b-v3` on first Parakeet run. `recall update` does not install that CLI. Parakeet weights are NVIDIA **CC-BY-4.0**; credit NVIDIA / the model in user-facing docs only when that engine ran. The `parakeet-mlx` runtime is Apache-2.0. Do not commit model weights.

If the Parakeet binary is missing, the error includes an install hint. Whisper still works with `--engine whisper`.

Before over-tuning merge heuristics, also test a larger local Whisper model:

```sh
recall transcribe latest --model models/ggml-small.en.bin
recall transcribe latest --model models/ggml-medium.en.bin
```

Larger models cost more local compute time but should improve transcript quality.

## Live Transcript

While recording, Apple SpeechAnalyzer can stream a Voice Memos-style preview into the Live Recall pane. Mic and Call are labeled separately. `t` hides or shows the block. Space mute stops feeding live mic samples (the archive still writes silence); call live continues. Enter ends capture immediately, then batch Apple `transcribe-file` writes canonical `transcript.md`. Live text is a preview, not the published transcript.

Default is on. Disable with `--no-live-transcript` or:

```toml
[live_transcription]
enabled = false
```

If SpeechAnalyzer fails, recording continues and Recall toasts that live text is unavailable.

## Current Behavior

The TUI starts transcription automatically after the user presses Enter to end a recording. The status area shows current transcript progress, including the active track/chunk, and shows the final `transcript.md` path when complete.

If you stay in that TUI session, Enter follows the header `next:` indicator (`s` toggles it, like consent). `next: append` records another numbered take in the same folder (`mic-002.m4a` / `call-002.m4a`). `next: new` starts a new session folder. Ending an appended take concatenates every take in order and re-transcribes the full mic and call tracks. Transcript timestamps follow concatenated audio time, not the TUI clock; the TUI clock includes the break between takes, and the audio does not insert silence for that gap.

A continued take can start while take 1 is still transcribing or analyzing. Those jobs keep running. The later take's full re-transcribe and analysis replace `transcript.md` and `meeting.md`. A late take-1 completion does not overwrite newer take outputs. Notes and markers accumulate.

Quitting the TUI (`q` / Ctrl+C) leaves transcription and analysis running in the background. Recall prints the session path and a log file under `.recall/work/postprocess.log`. Plain `recall` starts a new session. `recall --resume` or `recall --resume <session-id>` reopens the same folder so a later take can append. Resume does not auto-transcribe until that new take ends. The TUI clock restores the last accumulated recording time and does not add the time spent away.

Recall can keep processing a finished session while the user starts another recording. Background transcription and analysis jobs are session-scoped, so a previous session finishing should not overwrite the currently active session display. Continued takes use the same session-scoped rule, keyed by take generation.

The direct command still exists for re-running or debugging transcription:

```sh
recall transcribe latest
recall transcribe /path/to/session
```

End-state behavior should be:

1. User ends a recording in the TUI.
2. Recall finalizes the current take (`audio/mic-001.m4a` and `audio/call-001.m4a`, plus convenience `mic.m4a` / `call.m4a` aliases).
3. Recall automatically starts transcription.
4. Recall writes `transcript.md`.
5. If auto-analysis is enabled, Recall runs the selected headless agent and writes one complete `meeting.md` from the transcript.

## Agent Analysis

Recall can pass the clean `transcript.md` to a headless CLI agent and write one complete meeting record.

Manual analysis:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
recall analyze /path/to/session --agent codex
```

Dry run:

```sh
recall analyze latest --agent grok --dry-run
```

Supported built-in agent profiles:

```sh
recall agents list
recall agents doctor
```

Automatic TUI analysis:

```sh
recall --agent grok --auto-analyze
```

Or with a personal alias:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

Config defaults:

```toml
# ~/.config/recall/config.toml
consent_default = "provided"
storage_dir = "~/Documents/Recall/sessions"

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"

[transcription]
engine = "apple"      # default on Apple Silicon macOS 26+; fallback: "parakeet", then "whisper"
ffmpeg_bin = "~/Documents/Recall/tools/ffmpeg/bin/ffmpeg"
whisper_bin = "~/Documents/Recall/tools/whisper/bin/whisper-cli"
model_path = "~/Documents/Recall/models/ggml-base.en.bin"
parakeet_bin = "parakeet-mlx"
parakeet_model = "mlx-community/parakeet-tdt-0.6b-v3"
chunk_seconds = 600

[live_transcription]
enabled = true
```

Analysis outputs:

```text
meeting.md
.recall/analysis/
  prompt.md
  agent-raw-output.json or agent-raw-output.jsonl
  agent-result.json
```

Recall keeps control of file layout. The agent is asked to return one JSON object, including a concise title when possible. Recall uses that title to rename generic session folders, update metadata/headings, and render Markdown files from the normalized result.

## Required Local Tools

Recall currently expects:

- `ffmpeg`
- Apple SpeechAnalyzer on Apple Silicon macOS 26+ (default engine; on-device speech model)
- `parakeet-mlx` if you select `--engine parakeet` or Apple is unavailable
- Hugging Face cache of `mlx-community/parakeet-tdt-0.6b-v3` (downloaded on first Parakeet run)

Whisper fallback, still supported:

- `whisper-cli` from `whisper.cpp`
- a local ggml Whisper model file

See `docs/DEPENDENCIES.md` for the full dependency map, including no-Brew corporate setup.

Recall finds `ffmpeg` in this order:

1. `--ffmpeg <path>`
2. `RECALL_FFMPEG_BIN`
3. `[transcription].ffmpeg_bin` in `~/.config/recall/config.toml`
4. `tools/ffmpeg/bin/ffmpeg`
5. `ffmpeg` on `PATH`

`ffmpeg` is still a temporary dependency; the intended no-Brew direction is to replace it with Swift/AVFoundation audio conversion.

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
export RECALL_FFMPEG_BIN="$PWD/tools/ffmpeg/bin/ffmpeg"
export RECALL_WHISPER_BIN="$PWD/tools/whisper/bin/whisper-cli"
export RECALL_WHISPER_MODEL="$PWD/models/ggml-base.en.bin"
recall transcribe latest
```

If the model is not at `models/ggml-base.en.bin`, set:

```sh
export RECALL_WHISPER_MODEL=/path/to/ggml-model.bin
```

Parakeet:

```sh
uv tool install parakeet-mlx
export RECALL_PARAKEET_BIN=/path/to/parakeet-mlx   # only if it is not on PATH
export RECALL_PARAKEET_MODEL=mlx-community/parakeet-tdt-0.6b-v3
recall transcribe latest --engine parakeet
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

Recall transcribes both tracks separately, then writes a clean merged `transcript.md` for normal use. The separate per-track transcripts are kept in `.recall/transcription/raw-tracks.md`.

This preserves source separation while still giving future summarization code one obvious input file. Later work can add stronger speaker labels.
