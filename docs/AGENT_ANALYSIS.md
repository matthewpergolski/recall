# Agent Analysis

Recall can pass the clean `transcript.md` to a headless CLI agent and write one complete meeting record.

This is optional. Recording and local transcription still work without an agent.

What is sent to the agent: the transcript, and the text of the notes and markers you made during the session, each with its time. A picture pasted into a note is sent as its file name only; the picture itself is not sent. Nothing else in the session folder is sent. With no agent selected and auto-analyze off, nothing is sent at all.

## Supported Agents

Built-in profiles:

| Agent | Command Recall Runs | Expected Output |
| --- | --- | --- |
| Grok | `grok -p <prompt> --output-format json` | JSON |
| Cline | `cline --json <prompt>` | NDJSON |
| Codex | `codex exec --json <prompt>` | JSON |
| Claude | `claude --bare -p <prompt> --output-format json` | JSON |
| OpenCode | `opencode run --format json --pure --title Recall analysis --file <transcript.md> <prompt>` | NDJSON events |
| Pi | `pi --mode json --no-session <prompt>` | NDJSON events |

Check what is available on your machine:

```sh
recall agents list
recall agents doctor
```

OpenCode and Pi are optional extra CLIs. Pi is not required unless you want `--agent pi`.

Install Pi:

```sh
npm install -g --ignore-scripts @earendil-works/pi-coding-agent
```

OpenCode is whatever `opencode` binary is already on your `PATH`. Authenticate it the same way you would for interactive use (`opencode auth login`).

## Manual Analysis

Run analysis for the newest session:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
recall analyze latest --agent opencode
recall analyze latest --agent pi
```

Run analysis for a specific session:

```sh
recall analyze /path/to/session --agent codex
```

Preview the prompt without running the agent:

```sh
recall analyze latest --agent grok --dry-run
```

## TUI Auto-Analysis

Start Recall with an agent. TUI auto-analysis is on by default when an agent is selected:

```sh
recall --agent grok
```

Flow:

1. Start recording.
2. Press Enter to end.
3. Recall finalizes audio.
4. Recall transcribes locally.
5. Recall runs the selected agent.
6. Recall checks each time the agent cited against the lines of `transcript.md`.
7. Recall writes `meeting.md` with the summary, decisions, actions, questions, follow-ups, notes, and markers.

If another recording starts before analysis finishes, the previous session keeps processing in the background. Agent results are written back to that session folder and do not retarget the active recording.

If you continue the same TUI session after ending a take, analysis is a do-over on the latest full `transcript.md`. It replaces `meeting.md` and does not merge two meeting documents. Notes and markers stay. After the first continue, the session folder name stays sticky so a later generated title does not rename the folder mid-meeting.

Disable auto-analysis for a run:

```sh
recall --agent grok --no-auto-analyze
```

Choose a prompt preset:

```sh
recall --agent claude --preset work
```

## Alias Setup

Personal convenience alias:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

This is supported. Recall parses leading TUI defaults before subcommands, so commands like these still work:

```sh
recall list
recall transcribe latest
recall analyze latest --dry-run
```

If you add the alias to `~/.zshrc`, reload your shell:

```sh
source ~/.zshrc
```

## Config Setup

Create:

```text
~/.config/recall/config.toml
```

Example:

```toml
consent_default = "provided"
storage_dir = "~/Documents/Recall/sessions"
timezone = "America/Chicago"
keep_audio = true

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"

[transcription]
engine = "apple"      # default on Apple Silicon macOS 26+
ffmpeg_bin = "~/Documents/Recall/tools/ffmpeg/bin/ffmpeg"
whisper_bin = "~/Documents/Recall/tools/whisper/bin/whisper-cli"
model_path = "~/Documents/Recall/models/ggml-base.en.bin"
chunk_seconds = 600
```

With this config, plain `recall` starts with consent noted, writes sessions to the configured directory, uses explicit local transcription tools, and auto-analysis is enabled.

CLI flags override config:

```sh
recall --agent cline --auto-analyze
recall --agent grok --no-auto-analyze
```

## Output Files

Analysis writes:

```text
meeting.md
.recall/analysis/
  prompt.md
  agent-raw-output.json or agent-raw-output.jsonl
  agent-result.json
```

Each decision, action item, and question in `meeting.md` says where it came from:

- `(00:34, call)`: the cited time falls on that line of the transcript. If the agent also quoted words, they are in that line or the lines beside it. The time and speaker shown are the line's own.
- `(00:34, call; quote not found there)`: the time falls on a line, but the words the agent quoted are not there, and no single other line holds them.
- `(not found in transcript)`: the agent cited a time that no line holds, and its quote did not pick out exactly one line.
- `(note 12:10)`: no transcript line holds the quoted words, and a note of yours does. The time is the note's.
- `(no source given)`: the agent cited no time.

A quote found in exactly one other line moves the citation to that line. A follow-up may have no source; one that cites a time is checked the same way. The line under the title counts the result, for example "7 of 9 items cite a line of the transcript. 1 quote was not found where cited."

The mark says where to look. It does not say the item is right. Recall checks that a time is on a line and that quoted words were said; it cannot tell whether the agent read the line correctly. An open question carries no quote, so only its time is checked. Recall never drops an item and never invents a time. A transcript with no timed lines is not checked, and the notes say so.

What the agent is told about your notes:

- They are extra material beside the transcript. It uses a note when it helps, for example to get a name, an owner, or a date right, or to add a fact nobody said aloud, and ignores one that adds nothing.
- For a name, a spelling, an owner, or a date, a note wins over the transcript, because speech recognition mishears names. A disagreement about what was decided goes under open questions.
- The summary does not mention the notes or repeat them. They still appear under "Notes and Markers" in `meeting.md`, as before.
- A marker has no words; it flags a moment, and the lines near it may matter.
- A session with no notes and no markers gets the same prompt as before.

What the agent is told about doubtful lines:

- The list of mic lines left out for having no speech under them stays in `transcript.md` for a person to read. Recall cuts it from the copy the agent is sent, so a made-up line cannot become a decision or an action item.
- The prompt warns that the conversation itself can still hold lines nobody said, with examples: a short stray mic line during silence or while a call connects, a long rambling mic line that fits nothing around it, and a mic line that repeats the call line beside it with odd words. The agent is told not to base an item on such a line alone, and to use the call line when the mic echoes it.

`meeting.md` is the primary human-facing result. `transcript.md` is the clean source of truth and remains separate so long meetings do not make the meeting record unwieldy. Agents are instructed not to use `.recall/transcription/` unless explicitly asked.

## JSON Contract

Recall asks agents to return one JSON object:

```json
{
  "title": "Concise meeting title, no date.",
  "summary": "Concise meeting summary.",
  "decisions": [],
  "action_items": [],
  "questions": [],
  "followups": []
}
```

Recall keeps control of file layout. It parses the agent response, saves raw and normalized output under `.recall/analysis/`, renames generic session folders such as `quick-capture` when the agent returns a useful title, updates session metadata/headings, and renders `meeting.md` from that normalized result.

## Current Limits

- Agent parsing is initial and should be validated against real Grok/Cline/Codex/Claude/OpenCode/Pi output.
- Headless agents may use network services depending on how those tools are configured.
- Bad transcripts produce bad summaries, so continue to review `transcript.md`.
