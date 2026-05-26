# Moving Recall

This project can be moved to a new directory. There is no required `.zshrc` alias and no hard-coded workspace path in the Rust app.

## After Moving The Folder

From the new project directory:

```sh
cargo check
cargo run -- sources
cd capture-helper
swift build
swift run recall-capture list-sources
```

Then return to the repo root:

```sh
cd ..
cargo run
```

## Running Without Installing

From the repo root:

```sh
cargo run
cargo run -- --consent provided
```

This is enough for development. No alias is required.

## Installing `recall`

From the repo root:

```sh
cargo install --path .
```

After that, `recall` should be available from any terminal if Cargo's bin directory is on your `PATH`.

Check:

```sh
which recall
recall --version
```

Cargo usually installs binaries into:

```text
~/.cargo/bin
```

If `which recall` does not find it, add this to your shell config:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

For zsh, that usually goes in:

```text
~/.zshrc
```

## Optional Shortcuts

This is optional. It just opens Recall with consent already marked.

```sh
alias recall-ready='recall --consent provided'
```

If you add that alias to `~/.zshrc`, open a new terminal or run:

```sh
source ~/.zshrc
```

Then use:

```sh
recall-ready
```

Plain `recall` can also be aliased with defaults. Recall parses leading TUI defaults before subcommands, so this still preserves commands like `recall list` and `recall transcribe latest`:

```sh
alias recall='command recall --consent provided --agent grok --auto-analyze'
```

For longer-term defaults, prefer `~/.config/recall/config.toml`; see `docs/AGENT_ANALYSIS.md`.

## Reinstall After Moving

If you installed Recall with `cargo install --path .` and later move the project, reinstall from the new location:

```sh
cargo install --path .
```

The installed binary embeds the current package build, so reinstalling keeps your global `recall` command aligned with the moved source tree.

## Session Output

By default, sessions are written relative to the directory where you run Recall:

```text
sessions/
  <MM-DD-YYYY_H-MMapm>-et-quick-capture/
    audio/
      mic.m4a
```

If you run the installed `recall` command from another directory, it will create `sessions/` in that current directory. Current config supports consent and analysis defaults; explicit storage-location config is still future work.

Session IDs use Eastern Time for the timestamp prefix and include `et` in the folder name so names are stable for the user's preferred meeting timezone even when the project is run from another local timezone. If agent analysis returns a useful title, Recall can rename a generic folder such as `05-26-2026_7-21pm-et-quick-capture` to a topic-based name such as `05-26-2026_7-21pm-et-rain-birthdays-and-jersey-mikes-chat`.

## Current Caveat

The Rust app finds the Swift helper relative to the source repo at build time. During active development, run from the repo or reinstall with `cargo install --path .` after moving. Packaging the Swift helper with installed releases is a future task.
