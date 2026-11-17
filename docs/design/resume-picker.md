# Resume picker

Recall can open a resume picker on `recall resume` when you do not pass a session id. The picker lists meetings for the current sessions folder, newest first, and can widen to every folder Recall has used, to the current git branch, or to the other worktrees of this checkout. This document is the design and the build order. It does not change the program. The baseline is commit `76fd97c`, crate version 0.7.5.

Implementation waits for an explicit yes. Each pull request below starts from a test that fails, then makes that test pass.

## What changes

Four pull requests land in this order.

- PR 1 records the launch directory and git identity on new sessions. It branches from `main`.
- PR 2 adds the session catalog and makes `recall resume` with no id print that catalog when stdout is not a terminal. It branches from `main`. It does not depend on PR 1. Rows with no recorded branch stay blank.
- PR 3 draws the picker and, on a terminal, `recall resume` with no id opens it. It branches from PR 2.
- PR 4 adds preview and rename. It branches from PR 3.

PR 3 and PR 4 change keys a person presses. Each waits for Matthew's review before merge. PR 1 and PR 2 do not.

`recall --resume` with no id keeps meaning the latest session. `recall resume latest` and `recall resume <session-id>` keep their current meaning. `recall list` stays a plain path list.

## How a session is stored

A session is a directory of files. There is no index.

The default storage directory is `<current working directory>/sessions`, from `default_storage_dir` and `StartOptions::default_for_cwd` in `src/session.rs`. `storage_dir` in `~/.config/recall/config.toml` (or `$XDG_CONFIG_HOME/recall/config.toml`) replaces that default. `--storage` replaces it for one command. `start_session` creates the folder, `audio/`, and `.recall/`, then writes the files.

The folder name is a 12-digit inverted UTC key, a local timestamp, a zone token, and a slug of the title. `docs/SPEC.md` describes that name. Older folder names remain valid ids.

`write_session_files` writes these files.

- `.recall/metadata.json` is the source of truth for id, title, `created_at_unix`, status, and consent. `session_json` writes it as text. Readers use `serde_json::Value` and look up single keys. Extra keys stay unread. Older sessions may still have `recall.json` at the folder root. `metadata_path` prefers the current file.
- `meeting.md` is the note. It starts with a heading and a TL;DR section that says the analysis is pending.
- `transcript.md`, `.recall/markers.md`, and `.recall/notes.md` start as placeholders.
- Audio lands later as `audio/mic.m4a`, `audio/call.m4a`, and numbered takes such as `audio/mic-001.m4a`.
- `.recall/state/capture.json` is `CaptureProgress` in `src/audio.rs`. It holds `take_count`, `completed_take`, `elapsed_ms`, and `transcribed_take`.
- `.recall/analysis/overview.json` holds the TL;DR the TUI shows again. `saved_overview` in `src/analysis.rs` returns it only when its take matches `completed_take`.

`metadata.json` does not store the working directory, the git toplevel, the branch, the commit, the worktree, or a pull request. A search of `src/` for those fields on a session finds none. The `sources` object in `session_json` is written as null and is not updated when audio files appear. Audio exists when the files exist. `keep_audio = false` can delete the `m4a` files after a transcript, so a later listing can show no audio for a session that was recorded.

The title is the `title` string in `metadata.json`. Analysis may replace it. `update_session_title_files` writes the new title into metadata, and into the first line of the transcript, markers, and notes when those lines still have their original prefix. It parses metadata as a JSON object and inserts `title` and `id`, so other keys stay. It does not edit `meeting.md`. The analysis writer rewrites `meeting.md` separately.

`recall analyze` with no `--generation` renames the folder before that write, so the stored `id` matches the new folder. The TUI passes a generation, and that path skips the rename. After a published first take the TUI calls `maybe_rename_session_dir_for_title` itself, which only renames the directory. Metadata `id` can still name the old folder. `session_folder_is_sticky` is true when `take_count` is greater than 1, and then the folder name stays. The resume id a person types is the folder name.

Recall remembers every sessions folder it has used in `~/.config/recall/session-places.txt`, one path per line. `src/places.rs` says the file holds paths only. Temp directories are not added. `recall list` scans the current place with `list_sessions`, which keeps directories that contain metadata and sorts them by `created_at_unix` descending, then by path descending. `recall list --all` prints every place, and counts places that are missing.

## How resume works today

Resume is a flag and a subcommand. Both end in the same TUI.

`recall --resume` with no id, and `recall --resume latest`, set `ResumeTarget::Latest`. The test `leading_resume_without_target_means_latest` in `src/main.rs` locks the flag. `recall --resume <session-id>` sets `ResumeTarget::Named`. Help text says the flag reopens a session so Enter can record another take.

`recall resume` is `run_resume` then `parse_resume_args`. With no id it also selects latest, then calls `run_tui_with_options`. `recall resume latest` and `recall resume <session-id>` do the same with an explicit target. `recall resume --list` exits 2 with the text `Session picker is not implemented. Use recall list, then recall --resume <session-id>.` The test `resume_subcommand_list_explains_how_to_pick` in `tests/cli_smoke.rs` locks that error.

`prepare_resume` in `src/tui.rs` resolves the id through `find_session_target`. The current place wins. A unique name in another place is used, and the TUI says where it was found. `latest` and filesystem paths never leave the current place. Several matches resume none and list up to five names. `resume_block_reason` then refuses a live capture lock or an unfinished take. The TUI opens in the ended state with the saved title, consent, elapsed time, and overview.

The recording screen is `App` in `src/tui.rs`, 6910 lines at this commit. It uses ratatui `List` and `Paragraph`. It has no session list and no search box. Space mutes the mic. `r` refreshes capture sources. Esc cancels a note. Those keys are why the picker is a separate screen, not a new state inside `App`.

Quitting the TUI prints `resume_hint`, a `recall --resume <id>` command, and `recall --resume latest`.

A missing session fails before the terminal is set up. The process prints `Recall could not resume` and exits 1. An unfinished take does the same and does not delete the folder.

## Recommended design

The picker is a catalog plus a screen. The catalog reads session folders and returns rows. The screen paints rows and returns the chosen path, or nothing if you cancel. `main` then calls the existing `prepare_resume` path. The recording TUI does not learn a new mode.

### Recorded context

PR 1 adds one object to `metadata.json` at `start_session`, inside `session_json`.

```json
"context": {
  "cwd": "/Users/matthew/src/cairnkit",
  "git_toplevel": "/Users/matthew/src/cairnkit",
  "git_common_dir": "/Users/matthew/src/cairnkit/.git",
  "branch": "release/0.3.2"
}
```

`cwd` is the canonical directory Recall was launched from. `git_toplevel` is the canonical worktree root. `git_common_dir` is the canonical path of `git rev-parse --git-common-dir`, so linked worktrees of one repo share it and a normal checkout does not share it with another repo. `branch` is `git branch --show-current`. Detached HEAD, a missing git binary, and a directory that is not a repo store JSON null for the git fields. `cwd` is still stored. A git failure does not fail `start_session`.

Paths are absolute. `git rev-parse --git-common-dir` often prints `.git`. Resolve that against the toplevel before saving it. Do not store the commit, the remote URL, or a pull request. Do not call the network.

The title rewriter already keeps unknown JSON keys. A test must lock that, because a future rewrite that rebuilds `session_json` from scratch would drop `context`.

On this Linux checkout, five runs of `git rev-parse --show-toplevel`, `git rev-parse --git-common-dir`, and `git branch --show-current` each took between 0.77 ms and 1.16 ms. Three runs outside a repo returned exit code 128 in 0.68 ms to 1.37 ms. Calling git during start is acceptable. PR 1 still remeasures on the Mac and fails if the median of the three commands together exceeds 50 ms.

### What an old session can show

No migration. Absence of `context` means unknown, not an empty string.

These values are already on disk.

- Title, from metadata, or `Recall Session` when the title is missing.
- Age, from `created_at_unix`. A missing timestamp sorts last and the row shows no age. Do not render Unix epoch as a huge age. `list_sessions` uses 0 today. The picker must not copy that into the label.
- Duration, from `elapsed_ms` in `capture.json`. Hide it when the file is missing.
- Size, the sum of file lengths under the session directory. Compute it while listing. Do not read file bytes. `directory_size` in `src/transcription.rs` walks a tree the same way, and it is private to the Parakeet cache. The catalog does not call it.
- Microphone and call audio, from a non-empty file `discover_track_segments` already treats as that track. That includes `mic-001.m4a`, `mic-001-part-02.m4a`, `mic.m4a`, and the matching `call` names.
- TL;DR, from `overview.json` under the same take rule as `saved_overview`.

Branch at record time cannot be inferred. Do not write today's branch into an old file. A live git query on the sessions folder can still group that folder with a worktree, because the folder is still inside the checkout. That query is for filtering. It is not stored, and it is not shown as the session's branch.

A moved or renamed repo leaves the old path in `session-places.txt`. If that path is gone, the picker skips it and counts it, the same way `recall list --all` does. It does not search the disk. Running Recall in the new directory remembers the new place. Sessions that moved with the directory show up there. The stale list entry remains until `recall places forget`.

A shared `storage_dir` in config puts every repo's sessions in one folder. The place filter cannot split them. Recorded `context` can, and only for sessions created after PR 1.

### Scope

The default scope is the current sessions folder. That is the folder `recall list` and `recall --resume latest` already use. It is the working directory's `sessions` folder, unless config or `--storage` selected another one.

That default is deliberate. Recall stores a meeting in the directory where you launched it. A git toplevel is a property of some meetings, not the storage key. Launching in `repo/docs` already creates `repo/docs/sessions`, and `recall list` from the repo root does not show those sessions. The picker default matches that.

Three toggles widen the default. They are fields on the catalog request, not scattered booleans in the screen.

- All projects adds every path in `session-places.txt`. This is `recall list --all`.
- Current branch keeps sessions whose recorded `branch` equals `git branch --show-current` in the directory where you launched the picker. Sessions with a null branch drop out. The footer shows how many dropped out. Old sessions drop out of this toggle.
- All worktrees adds other places whose live git common dir equals the common dir of the launch directory. A `repo/docs/sessions` folder is included, because git run in that folder still reports the repo. Places that are not inside a repo are left out. When all projects is on, this toggle adds nothing, because all projects is already wider.

Subdirectories of one checkout stay out of the default list. The worktree toggle is how you see them. A renamed repo is the moved-repo case above.

### Search, sort, preview, and rename

Search is a case-insensitive substring over the title, the short folder id (`short_session_id`), and the TL;DR. An empty query shows every row in scope. No match shows one line and resumes nothing. The catalog does not read `transcript.md`. Search across full transcripts is already a later item in `docs/SPEC.md`, and a transcript can be large.

Sort is `created_at_unix` descending, then path descending. That is the `list_sessions` order. There is no other sort.

Preview is a pure string. It contains the title, the local start time, the duration when known, the branch or the words `branch not recorded`, the audio marks, and the first line of the TL;DR up to 240 characters. It does not contain the transcript body. A test can pin that.

Rename edits the title. Enter saves. Esc cancels the edit and leaves the files alone. An empty title is rejected. On a one-take session the save renames the folder first, then writes the title files, and sets metadata `id` to the new folder name. That is the order `recall analyze` uses when no generation is set. The TUI order leaves `id` on the old name, and the picker does not copy that. The title write covers metadata `title`, the first line of `meeting.md` when that line starts with `# `, and the same first-line updates `update_session_title_files` already does for the transcript, markers, and notes. The selection follows the new path. A folder with more than one take keeps its name. The body of `meeting.md` under the heading stays. Audio bytes stay. If the write fails, the picker shows the error and keeps the old title.

### Row fields

Each row is two lines. The first line is the title. The second line is the age, the duration, the size, and the branch when one was recorded. Short marks at the end of the second line say `mic`, `call`, and `tldr` when those artifacts exist.

Size uses the existing `format_bytes` scale so the row matches other Recall output. Age is a short relative phrase from `created_at_unix`. Duration is `elapsed_ms`, rendered as `m:ss` under an hour and `h:mm` otherwise.

Do not show a pull request. Recall does not store one, and looking one up would use the network. A meeting is often not a branch on GitHub.

A row whose `resume_block_reason` is set still appears. The footer shows that reason when the row is selected. Enter still calls `prepare_resume`, which returns the same error as today.

### Where the picker runs

`recall resume` with no session id opens the picker when stdout is a terminal. Esc cancels, exits 0, and creates no session. Enter returns the session path. `main` passes that path to the existing resume path. A full path is safe, because `resolve_session_target` already accepts an existing session directory.

When stdout is not a terminal, `recall resume` with no id prints the default-scope rows and exits 2. The print path returns before ratatui starts. The text tells you to pass a session id. `tests/cli_smoke.rs` can assert that, because cargo tests capture stdout and have no terminal.

`recall resume --list` always prints those rows and does not open the screen. It exits 0 when the catalog was readable, including when the list is empty. A session id together with `--list` is an error, exit 2.

The screen is a new `src/resume_picker.rs`. It owns a ratatui `List` with `ListState`, a `Paragraph` for the query, a `Paragraph` for the footer, and a `Paragraph` for the preview. Each row is a two-line `ListItem`, in the shape of the Claude Code resume screen. The event loop lives in that file.

The screen handles these keys.

- Typing and Backspace edit the query.
- Up and Down move the selection.
- Enter resumes the selected session.
- Esc closes the preview if it is open, cancels a rename if one is open, and otherwise leaves the picker.
- Ctrl+A toggles all projects.
- Ctrl+B toggles the current branch.
- Ctrl+W toggles all worktrees.
- Space toggles the preview.
- Ctrl+R starts a rename.
- Ctrl+C leaves the picker.

`recall --resume` is unchanged. The quit hint can keep printing `recall --resume <id>`. Help text gains a line for `recall resume` and for `recall resume --list`.

## Other approaches

Three shapes were compared. The recommendation is the first.

### Scan session folders when the picker opens

`list_sessions` already does this for one folder. The catalog adds a title read, `capture.json`, a size walk, and a check for `overview.json`. There is one source of truth, the session directory. A hand-deleted folder disappears on the next launch. The cost is about four files and on the order of 1000 lines across the four pull requests. `src/resume_catalog.rs` is the new catalog, roughly 250 lines plus tests. `src/resume_picker.rs` is the screen, roughly 350 lines plus tests. Context in `src/session.rs` is roughly 80 lines. The command branch in `src/main.rs` is roughly 50 lines. Those counts are estimates from the size of `list_sessions` and `prepare_resume`. The code is not written yet.

### Keep an index next to session-places.txt

Launch would read one file. Start, analysis, title edits, folder renames, and quit would all update it, under the same kind of lock `places.rs` already uses. A crash, or a folder deleted by hand, leaves the index wrong. The measurement below leaves the index out.

### Add a picker state to App

Terminal setup would be shared with the recording screen. `src/tui.rs` is 6910 lines at this commit. Space, `r`, and Ctrl+C already do other work there. A picker fault would sit in the recording loop. A separate screen returns a path, and then the current TUI runs as it does today.

## Measurement

The decision before the run was this. If a release scan of 1000 session folders stays under 200 ms, the picker reads the folders on each launch. An index is worth a second file only if the median crosses 500 ms.

The probe was a throwaway release binary in a temp directory, and it is not part of this repo. It used the standard library only. It read the title and the timestamps with a short string search, and it did not use `serde_json`. For each session it read `metadata.json` and `capture.json`, recorded the length of each file in `audio/`, and checked that `overview.json` exists. It left `transcript.md` and the audio bytes unread. The fixture files were 1 KB and 2 KB, on ext4 at `/dev/vdc`. The machine had 4 cores and a load average of 0.04. The host was this Linux VM.

Warm runs alternated a full scan and a one-file index read, five times, after one warmup that is not in the median. Every run counted the right number of sessions, and the scan checksum matched the index checksum. No run returned an error.

| Sessions | Scan median | Scan range | Index median | Index range |
| --- | --- | --- | --- | --- |
| 50 | 0.36 ms | 0.35 ms to 0.66 ms | 0.006 ms | 0.005 ms to 0.008 ms |
| 200 | 1.6 ms | 1.6 ms to 1.8 ms | 0.021 ms | 0.020 ms to 0.022 ms |
| 1000 | 8.6 ms | 8.5 ms to 8.8 ms | 0.095 ms | 0.091 ms to 0.104 ms |

The 50-session scan range is wide because the first of the five runs was 0.66 ms and the other four were 0.35 ms to 0.40 ms.

Cold scans dropped the page cache before each of five runs of the 1000-session scan. Those runs were 73 ms, 192 ms, 151 ms, 163 ms, and 173 ms. The median is 163 ms. The range is 73 ms to 192 ms. All five are under 200 ms. The cold index was not measured. `strace` and `perf` were not installed, so there is no syscall profile. The 1000-session scan read 177090 bytes of JSON. At 163 ms that is about 1 MB/s, which is far below disk bandwidth, so the cold time is the wait on many small reads. Parsing with `serde_json` was not measured. It would add time on the warm path and would not move a cold run that is waiting on the disk.

The warm result is what a second open costs. The cold result is a first open after the cache is empty. Both stay under the 200 ms bar at 1000 sessions, which is already a large library for this app. The index saves about 8 ms warm and would save some of the 163 ms cold. That does not earn a second source of truth. Revisit the index if a real library reaches several thousand sessions, or if a cold scan on the Mac passes 500 ms.

## Build order

Each pull request bumps the patch version by 1 against its base, and updates `Cargo.lock` to match. Docs-only commits do not. The check for a source pull request is `./scripts/check.sh`. A timing check is a probe, not an assertion inside `cargo test`, so a slow machine does not fail the suite.

### PR 1. Record launch context

- [ ] Depends on nothing. Branch from `main`.
- [ ] Edits `src/session.rs`, a regression test in `src/analysis.rs`, the metadata sketch in `docs/SPEC.md`, and the crate version.
- [ ] Builds the `context` object on new sessions, and nothing else.
- [ ] The result is a new session whose `.recall/metadata.json` contains `cwd` and either git fields or nulls. An analysis title update leaves `context` in the file. The folder rename rules stay as they are.
- [ ] Unit. Write `new_session_records_cwd_and_git_context` first. It fails because `session_json` has no `context`. Then make it pass. Add `title_rewrite_keeps_context`, which fails if a title update drops the object.
- [ ] Live. `cargo run -- start --title "Context check" --storage <temp>` exits 0, and the metadata `cwd` is the directory you launched from. Git fields are filled in this repo and null in `/tmp`. Delete the temp session afterward.
- [ ] Perf. Metric is the time of the three git commands during start. Probe is those commands, 10 runs, median. Trunk baseline on this Linux checkout is about 1 ms each. The number that fails the pull request is a median over 50 ms for the three together. Session start must still succeed when git exits non-zero.

### PR 2. Catalog and printed resume

- [ ] Depends on nothing. Branch from `main`. Recorded branch filters return no rows until PR 1 has shipped and new sessions exist. That is correct, not a blocker.
- [ ] Edits `src/resume_catalog.rs`, `src/main.rs`, `tests/cli_smoke.rs`, and the crate version.
- [ ] Builds the row type, the three toggles, search, and the non-terminal print.
- [ ] The result is a piped `recall resume` that prints titles for the current place, newest first, and exits 2. `recall resume --list` prints them and exits 0. The command opens no picker and creates no session folder.
- [ ] Unit. Write `resume_with_no_target_prints_sessions_when_stdout_is_not_a_terminal` first. It fails because today's command tries the TUI and does not print the title. Catalog tests then cover branch filtering, worktree filtering, search, and the rule that a row builder does not open `transcript.md`.
- [ ] Live. With two temp sessions, the piped command prints the newer title first and the older title second. A third session in another place appears only when the test calls the all-projects filter. The command creates no folder.
- [ ] Perf. Metric is warm catalog time at 200 sessions. Probe is the same shape as the measurement above. Trunk has no catalog. The baseline is the 1.6 ms warm scan in this document. The number that fails a later change is a warm 200-session catalog over 20 ms on a machine like this one, or a cold 1000-session catalog over 500 ms. PR 2 does not add the index.

After PR 2, change `resume_subcommand_list_explains_how_to_pick` to the new `--list` contract. Do that in the same pull request, after the new test has failed for the right reason.

### PR 3. Picker screen

- [ ] Depends on PR 2. Branch from PR 2.
- [ ] Edits `src/resume_picker.rs`, the launch branch in `src/main.rs`, the help text, `README.md`, the resume section of `docs/SPEC.md`, and the crate version.
- [ ] Builds the screen. Enter resumes. Esc cancels. The toggles and search call the PR 2 catalog.
- [ ] The result on a terminal is a `recall resume` that shows the list. Enter opens the ended TUI for that folder. Esc returns to the shell. Piped stdout still prints, as in PR 2.
- [ ] Unit. Write `escape_cancels_and_enter_selects_the_highlighted_session` first, against the picker state function. It fails to compile until that function exists. Keys in the test are Ctrl+A, Ctrl+B, Ctrl+W, and typing. The test does not open a terminal.
- [ ] Live. On a Mac, drive Ghostty with its AppleScript, as `AGENTS.md` describes. Use a temp `--storage`. Pass predicate. The new session's title is on screen, Enter reaches the ended dashboard for that same folder, and a second launch cancelled with Esc leaves no extra session folder. Close only the window the script opened. This cloud checkout has no terminal and cannot run this box.
- [ ] Perf. The wait before the first paint is the catalog build. Use the PR 2 gate. Fail the pull request if the picker opens `transcript.md`. A test can list the files the catalog opens.

This pull request waits for Matthew's review before merge.

### PR 4. Preview and rename

- [ ] Depends on PR 3. Branch from PR 3.
- [ ] Edits `src/resume_picker.rs` and the title-update path it calls. Make the title update callable without rewriting `meeting.md` from an analysis result. Bump the crate version.
- [ ] Builds Space to preview and Ctrl+R to rename.
- [ ] The result is that Space shows the TL;DR and then hides it. Ctrl+R changes the title on the next `recall resume --list`. A one-take folder may change name. A two-take folder keeps its name. The transcript body and the audio bytes stay as they were.
- [ ] Unit. Write `preview_shows_the_tldr_and_not_the_transcript` and `rename_updates_the_title_and_renames_only_a_single_take_folder` first. Both fail until the preview string and the rename function exist.
- [ ] Live. Ghostty, temp storage. Pass predicate. After Ctrl+R, `recall resume --list` prints the new title, and Space shows the fixture TL;DR. Esc during rename leaves the old title on disk.
- [ ] Perf. Rename does not read audio bytes. The test compares the `m4a` length and contents before and after. A rename that walks audio fails the test.

This pull request waits for Matthew's review before merge.

## Open questions

These are the choices a run cannot settle. The design above already picks one answer so the work can start. Matthew can overrule any of them.

1. Should `recall --resume` with no id open the picker as well? This design leaves it as latest, because help text, the quit hint, and `leading_resume_without_target_means_latest` all say latest. Claude Code uses the bare flag for the picker. Changing the flag is a one-branch edit later if you want that.
2. Should the branch toggle hide sessions that have no recorded branch, or list them under a separate heading? This design hides them and shows the count.
3. Should sessions launched from a subdirectory show up in the default list? This design leaves them out, and the worktree toggle includes them.
4. Do you want a pull request on the row later? This design leaves it out. A lookup uses the network, which Recall does not do for meeting data.
5. Should a picker rename follow the analysis rule and rename a one-take folder? This design says yes, so there is one rename rule.

## Left unproven

The picker has not been drawn. This checkout cannot drive the TUI. The scan numbers are from a Linux ext4 volume with synthetic 1 KB audio files, a hand-rolled JSON read, and no `strace` output. They are not a Mac measurement. The git timings are from this small checkout, not from a large repository on a Mac. Nobody has counted how many sessions a real `session-places.txt` holds.
