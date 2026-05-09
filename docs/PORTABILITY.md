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

## Optional Consent Shortcut

This is optional. It just opens Recall with consent already marked while keeping `recall sources`, `recall list`, and other subcommands normal.

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

If you want plain `recall` to open the TUI with consent marked while preserving subcommands, use a shell function instead of an alias:

```sh
recall() {
  if [ "$#" -eq 0 ]; then
    command recall --consent provided
  else
    command recall "$@"
  fi
}
```

Do not alias `recall` itself to `command recall --consent provided`; that makes `recall sources` expand into an invalid command shape.

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
  <timestamp>-quick-capture/
    audio/
      mic.m4a
```

If you run the installed `recall` command from another directory, it will create `sessions/` in that current directory. A future config file should make the storage location explicit.

## Current Caveat

The Rust app finds the Swift helper relative to the source repo at build time. During active development, run from the repo or reinstall with `cargo install --path .` after moving. Packaging the Swift helper with installed releases is a future task.
