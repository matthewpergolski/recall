# Agent Analysis

Recall can pass the clean `transcript.md` to a headless CLI agent and write structured meeting memory files.

This is optional. Recording and local transcription still work without an agent.

## Supported Agents

Built-in profiles:

| Agent | Command Recall Runs | Expected Output |
| --- | --- | --- |
| Grok | `grok -p <prompt> --output-format json` | JSON |
| Cline | `cline --json <prompt>` | NDJSON |
| Codex | `codex exec --json <prompt>` | JSON |
| Claude | `claude --bare -p <prompt> --output-format json` | JSON |

Check what is available on your machine:

```sh
recall agents list
recall agents doctor
```

## Manual Analysis

Run analysis for the newest session:

```sh
recall analyze latest --agent grok
recall analyze latest --agent cline
recall analyze latest --agent claude --preset work
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

Start Recall with an agent and automatic analysis:

```sh
recall --agent grok --auto-analyze
```

Flow:

1. Start recording.
2. Press `e` to end.
3. Recall finalizes audio.
4. Recall transcribes locally.
5. Recall runs the selected agent.
6. Recall writes summary/action files.

Disable auto-analysis for a run:

```sh
recall --agent grok --no-auto-analyze
```

Choose a prompt preset:

```sh
recall --agent claude --auto-analyze --preset work
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

[analysis]
default_agent = "grok"
auto_analyze = true
preset = "general"
```

With this config, plain `recall` starts with consent noted and auto-analysis enabled.

CLI flags override config:

```sh
recall --agent cline --auto-analyze
recall --agent grok --no-auto-analyze
```

## Output Files

Analysis writes:

```text
summary.md
actions.md
decisions.md
questions.md
followups.md
analysis-debug/
  prompt.md
  agent-raw-output.json or agent-raw-output.jsonl
  agent-result.json
```

`transcript.md` is the clean source of truth. Agents are instructed not to use `transcription-debug/` unless explicitly asked.

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

Recall keeps control of file layout. It parses the agent response, saves the raw output under `analysis-debug/`, stores normalized JSON as `agent-result.json`, renames generic session folders such as `quick-capture` when the agent returns a useful title, updates session metadata/headings, and renders Markdown files from that normalized result.

## Current Limits

- Agent parsing is initial and should be validated against real Grok/Cline/Codex/Claude output.
- Headless agents may use network services depending on how those tools are configured.
- Bad transcripts produce bad summaries, so continue to review `transcript.md`.
