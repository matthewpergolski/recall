# Git Privacy Checklist

Before making this repository public or sharing it broadly, do not commit local meeting data, generated binaries, or machine-specific artifacts.

## Do Not Commit

These paths are ignored by `.gitignore` and should stay local:

```text
target/
capture-helper/.build/
sessions/*
models/*
tools/*
.env
.env.*
*.m4a
*.wav
*.mp3
*.flac
*.vtt
*.log
```

## Why

- `sessions/` contains meeting audio, transcripts, summaries, metadata, and stop files.
- `models/` contains large Whisper model files such as `ggml-base.en.bin`.
- `tools/` may contain local or corporate-approved binaries.
- `target/` and `capture-helper/.build/` are generated build outputs.
- `.env` files may contain local paths, future API keys, or private configuration.

## Files To Review Before First Commit

Review these manually before publishing:

```sh
rg -n "Users/|<your-username>|1778|CleanShot|Visual Studio Code|Codex.app" . \
  --glob '!target/**' \
  --glob '!capture-helper/.build/**' \
  --glob '!sessions/**' \
  --glob '!models/**'
```

Also review `memory-bank/` before public release. It is useful for project continuity, but it can contain development history, local session IDs, and notes about user-specific testing.

## Safe To Commit

Generally safe:

```text
src/
capture-helper/Package.swift
capture-helper/Sources/
docs/
README.md
AGENTS.md
Cargo.toml
Cargo.lock
.gitignore
sessions/.gitkeep
models/.gitkeep
tools/.gitkeep
tools/whisper/.gitkeep
tools/whisper/bin/.gitkeep
```

For an application repository, committing `Cargo.lock` is recommended so builds are reproducible.
