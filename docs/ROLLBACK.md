# Going Back to an Earlier Version

Use this when a new version breaks something and you need the old behavior back on one Mac.

## List the versions

Run this in the Recall checkout:

```sh
uv run scripts/versions.py
```

It prints every released version, newest first, with its commit and date. It marks the version that is installed and the one that is checked out. A version is a commit on `origin/main` that changed `version` in `Cargo.toml`. Recall has no tags or GitHub releases yet.

## Get the commands for one version

```sh
uv run scripts/versions.py 0.6.4
```

The script prints the commands with this Mac's paths filled in. It only reads: it never changes the checkout or the installed command. You run the commands yourself.

## What the commands do

```sh
cd <the Recall checkout>
git status --short
git switch --detach <commit>
swift build --package-path capture-helper
cargo install --path . --locked
recall --version
```

1. `git status --short` must print nothing. Commit or set aside any work first; the next step refuses otherwise.
2. `git switch --detach <commit>` puts the checkout's files at that version. It does not move `main` and does not change GitHub.
3. `swift build` rebuilds the capture helper. The installed command runs the helper from inside the checkout, so the helper must match the version too.
4. `cargo install` replaces the `recall` command.

## Return to the newest version

```sh
cd <the Recall checkout>
git switch main
recall update
```

`recall update` refuses to run while the checkout sits on an older commit, so `git switch main` comes first. The update then sees that the installed command is older than `main`, runs the tests, rebuilds the helper, and reinstalls.

## What going back does not undo

- **Recorded sessions.** They are plain files under your storage folder, and no step here touches them. An older version ignores files it does not know, such as `.recall/timeline/`.
- **Other Macs.** Each Mac has its own checkout and its own installed command. Going back on one Mac changes nothing on another.
- **GitHub.** `origin/main` stays where it is, so `recall update` on any Mac still installs the newest version. To take a release back for every Mac, the fix is a new commit on `main` that undoes the change (`git revert <commit>`, then a normal push). Do not rewrite `main`'s history for this.
- **Your config** at `~/.config/recall/config.toml`. An older version may not know a newer setting.

## How this was checked

On 2026-10-06 the commands were run for 0.6.4 in a scratch clone, with the command installed into a separate folder: the helper built, the install finished, and `recall --version` printed `recall 0.6.4`. Then `git switch main` returned the clone to `main`. Not checked there: `recall update` as the return step, and versions older than 0.6.4, which may need an older Xcode or Rust.
