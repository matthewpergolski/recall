//! History classification and aside-and-clone repair for `recall update`.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::update::{
    git_at, git_output, note_lines, scrub_git_command, subjects_from_log, NOTE_LIMIT,
};

const OFFICIAL_CLONE: &str = "https://github.com/matthewpergolski/recall.git";

/// How local `main` relates to `origin/main` after a fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryClass {
    Current,
    FastForward,
    Unrelated { ahead: usize, behind: usize },
    Diverged { ahead: usize, behind: usize },
    LocalOnly { count: usize },
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // returned for callers/tests; update() only needs the side effects
pub(crate) struct AsideRepair {
    pub checkout: PathBuf,
    pub aside: PathBuf,
    pub unique_subjects: Vec<String>,
}

pub(crate) fn classify_history(checkout: &Path) -> io::Result<HistoryClass> {
    let head = git_output(checkout, &["rev-parse", "HEAD"])?;
    let origin = git_output(checkout, &["rev-parse", "refs/remotes/origin/main"])?;
    if head == origin {
        return Ok(HistoryClass::Current);
    }

    let merge_base = git_at(checkout)
        .args(["merge-base", "HEAD", "refs/remotes/origin/main"])
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Failed to run git merge-base: {error}"),
            )
        })?;

    let (ahead, behind) = ahead_behind(checkout)?;

    if !merge_base.status.success() {
        return Ok(HistoryClass::Unrelated { ahead, behind });
    }

    let base = String::from_utf8_lossy(&merge_base.stdout)
        .trim()
        .to_string();
    if base == head {
        return Ok(HistoryClass::FastForward);
    }
    if base == origin {
        return Ok(HistoryClass::LocalOnly { count: ahead });
    }
    Ok(HistoryClass::Diverged { ahead, behind })
}

fn ahead_behind(checkout: &Path) -> io::Result<(usize, usize)> {
    let raw = git_output(
        checkout,
        &[
            "rev-list",
            "--left-right",
            "--count",
            "HEAD...refs/remotes/origin/main",
        ],
    )?;
    let mut parts = raw.split_whitespace();
    let ahead = parts.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
    let behind = parts.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
    Ok((ahead, behind))
}

pub(crate) fn unique_local_subjects(checkout: &Path) -> io::Result<Vec<String>> {
    let output = git_at(checkout)
        .args([
            "log",
            "--reverse",
            "--no-show-signature",
            "--format=%s",
            "HEAD",
            "--not",
            "refs/remotes/origin/main",
        ])
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Failed to list unique local commits: {error}"),
            )
        })?;
    if !output.status.success() {
        // Unrelated histories: every local commit is unique.
        let fallback = git_output(
            checkout,
            &[
                "log",
                "--reverse",
                "--no-show-signature",
                "--format=%s",
                "HEAD",
            ],
        )?;
        return Ok(subjects_from_log(&fallback));
    }
    Ok(subjects_from_log(&String::from_utf8_lossy(&output.stdout)))
}

pub(crate) fn choose_aside_path(checkout: &Path, date: &str) -> io::Result<PathBuf> {
    let parent = checkout.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Checkout {} has no parent directory", checkout.display()),
        )
    })?;
    let name = checkout
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Checkout {} has no directory name", checkout.display()),
            )
        })?
        .to_string_lossy();
    let base = parent.join(format!("{name}-old-{date}"));
    if !base.exists() {
        return Ok(base);
    }
    for n in 2..10_000 {
        let candidate = parent.join(format!("{name}-old-{date}-{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::other(format!(
        "Could not find a free aside path near {}",
        base.display()
    )))
}

pub(crate) fn needs_aside_repair(class: &HistoryClass) -> bool {
    matches!(
        class,
        HistoryClass::Unrelated { .. }
            | HistoryClass::Diverged { .. }
            | HistoryClass::LocalOnly { .. }
    )
}

pub(crate) fn prompt_message(checkout: &Path, class: &HistoryClass, aside: &Path) -> String {
    match class {
        HistoryClass::Unrelated { ahead, behind } | HistoryClass::Diverged { ahead, behind } => {
            format!(
                "Checkout {} does not share history with origin/main.\n\
Local main is {ahead} commits ahead and {behind} commits behind origin/main.\n\
Move it to {} and clone a fresh copy into {}? [Y/n]",
                checkout.display(),
                aside.display(),
                checkout.display()
            )
        }
        HistoryClass::LocalOnly { count } => format!(
            "Local main has {count} commits that are not on origin/main.\n\
Move {} to {} and clone a fresh copy? [Y/n]",
            checkout.display(),
            aside.display()
        ),
        HistoryClass::Current | HistoryClass::FastForward => String::new(),
    }
}

pub(crate) fn noninteractive_message(checkout: &Path, class: &HistoryClass) -> String {
    match class {
        HistoryClass::LocalOnly { .. } => format!(
            "Local main has commits that are not on origin/main. Re-run with --yes to move {} aside and clone fresh.",
            checkout.display()
        ),
        _ => format!(
            "Checkout {} does not share history with origin/main. Re-run with --yes to move it aside and clone fresh.",
            checkout.display()
        ),
    }
}

pub(crate) fn confirm_aside(yes: bool, interactive: bool) -> io::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !interactive {
        return Ok(false);
    }
    let mut line = String::new();
    let bytes_read = io::stdin().lock().read_line(&mut line)?;
    Ok(accept_aside_answer(bytes_read, &line))
}

/// Interpret one `[Y/n]` read. Enter (newline → empty trim) is yes. EOF / Ctrl-D
/// (`bytes_read == 0`) is refuse. `y` / `yes` are yes; anything else is no.
pub(crate) fn accept_aside_answer(bytes_read: usize, line: &str) -> bool {
    if bytes_read == 0 {
        return false;
    }
    let answer = line.trim();
    answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

pub(crate) fn stdin_is_terminal() -> bool {
    io::stdin().is_terminal()
}

/// Move the broken checkout aside and clone a fresh tree into the original path.
///
/// Clones into a sibling temp directory first. The user's checkout is renamed
/// aside only after that clone succeeds, so a failed clone leaves the original
/// path untouched.
pub(crate) fn aside_and_clone<F>(
    checkout: &Path,
    date: &str,
    mut clone_into: F,
) -> io::Result<AsideRepair>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let aside = choose_aside_path(checkout, date)?;
    let unique_subjects = unique_local_subjects(checkout)?;
    let parent = checkout.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Checkout {} has no parent directory", checkout.display()),
        )
    })?;
    let aside_parent = aside.parent().unwrap_or(parent);
    if parent != aside_parent {
        return Err(io::Error::other(format!(
            "Cannot move {} to {}: paths do not share a parent.",
            checkout.display(),
            aside.display()
        )));
    }

    println!("Keeping local commits in {}", aside.display());
    let mut lines = note_lines(&unique_subjects, NOTE_LIMIT);
    if !lines.is_empty() {
        lines[0] = "Unique local commits".to_string();
        for line in &lines {
            println!("{line}");
        }
    }
    let _ = io::stdout().flush();

    let temp = choose_clone_temp(checkout)?;
    if let Err(error) = clone_into(&temp) {
        let _ = fs::remove_dir_all(&temp);
        return Err(error);
    }

    if let Err(error) = fs::rename(checkout, &aside) {
        let _ = fs::remove_dir_all(&temp);
        return Err(io::Error::new(
            error.kind(),
            format!(
                "Failed to move {} to {}: {error}",
                checkout.display(),
                aside.display()
            ),
        ));
    }
    println!("Moved {} to {}", checkout.display(), aside.display());

    if let Err(error) = fs::rename(&temp, checkout) {
        let restore = fs::rename(&aside, checkout);
        let restore_note = match &restore {
            Ok(()) => {
                let _ = fs::remove_dir_all(&temp);
                format!("Restored {} from {}.", checkout.display(), aside.display())
            }
            Err(restore_err) => format!(
                "Could not restore {checkout} from {aside}: {restore_err}. \
Local files are in {aside}. Clone is in {temp}.",
                checkout = checkout.display(),
                aside = aside.display(),
                temp = temp.display(),
            ),
        };
        return Err(io::Error::other(format!(
            "Failed to move clone from {} into {}: {error}. {restore_note}",
            temp.display(),
            checkout.display()
        )));
    }
    println!("Cloned origin/main into {}", checkout.display());

    Ok(AsideRepair {
        checkout: checkout.to_path_buf(),
        aside,
        unique_subjects,
    })
}

fn choose_clone_temp(checkout: &Path) -> io::Result<PathBuf> {
    let parent = checkout.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Checkout {} has no parent directory", checkout.display()),
        )
    })?;
    let name = checkout
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Checkout {} has no directory name", checkout.display()),
            )
        })?
        .to_string_lossy();
    let pid = std::process::id();
    for n in 0..10_000 {
        let candidate = parent.join(format!(".{name}.recall-clone-{pid}-{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::other(format!(
        "Could not find a free clone temp path near {}",
        parent.display()
    )))
}

pub(crate) fn clone_official(checkout: &Path) -> io::Result<()> {
    let mut command = Command::new("git");
    scrub_git_command(&mut command);
    let status = command
        .args(["clone", "--quiet", OFFICIAL_CLONE])
        .arg(checkout)
        .status()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("Failed to clone Recall: {error}"))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Failed to clone {OFFICIAL_CLONE} into {}",
            checkout.display()
        )))
    }
}

pub(crate) fn today_ymd() -> String {
    jiff::Zoned::now().date().to_string()
}

pub(crate) fn refuse_aside(checkout: &Path, class: &HistoryClass, interactive: bool) -> io::Error {
    if !interactive {
        return io::Error::other(noninteractive_message(checkout, class));
    }
    match class {
        HistoryClass::LocalOnly { .. } => io::Error::other(format!(
            "Left {} unchanged. Recall will not install commits that are only on the local main.",
            checkout.display()
        )),
        _ => io::Error::other(format!(
            "Left {} unchanged. Re-run with --yes to move it aside and clone fresh, or pass --repo <path> to use another checkout.",
            checkout.display()
        )),
    }
}

/// Run the aside-and-clone prompt and repair when history needs it.
///
/// `date` and `clone_into` are injectable so tests never hit the network.
pub(crate) fn maybe_repair_history<F>(
    checkout: &Path,
    class: &HistoryClass,
    yes: bool,
    date: &str,
    clone_into: F,
) -> io::Result<Option<AsideRepair>>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    if !needs_aside_repair(class) {
        return Ok(None);
    }
    let aside = choose_aside_path(checkout, date)?;
    let interactive = stdin_is_terminal();
    if !yes && !interactive {
        return Err(refuse_aside(checkout, class, false));
    }
    if !yes {
        println!("{}", prompt_message(checkout, class, &aside));
        if !confirm_aside(false, true)? {
            return Err(refuse_aside(checkout, class, true));
        }
    }
    let repair = aside_and_clone(checkout, date, clone_into)?;
    Ok(Some(repair))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::git_at;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "recall-repair-{}-{}-{id}",
                label,
                std::process::id()
            ));
            fs::create_dir_all(path.join("src")).unwrap();
            fs::create_dir_all(path.join("capture-helper")).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                "[package]\nname = \"recall\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
            fs::write(path.join("src/main.rs"), "fn main() {}\n").unwrap();
            fs::write(path.join("capture-helper/Package.swift"), "// fixture\n").unwrap();
            run(&path, &["init", "-b", "main"]);
            run(
                &path,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/matthewpergolski/recall.git",
                ],
            );
            Self { path }
        }

        fn commit(&self, message: &str) {
            run(&self.path, &["add", "."]);
            run(
                &self.path,
                &[
                    "-c",
                    "user.name=Recall Test",
                    "-c",
                    "user.email=recall@example.invalid",
                    "commit",
                    "-m",
                    message,
                ],
            );
        }

        fn mark_origin(&self) {
            run(
                &self.path,
                &["update-ref", "refs/remotes/origin/main", "HEAD"],
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run(path: &Path, args: &[&str]) {
        let output = git_at(path).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn stand_in_clone(src: &Path) -> impl FnMut(&Path) -> io::Result<()> + '_ {
        move |dest: &Path| copy_dir(src, dest)
    }

    fn copy_dir(src: &Path, dest: &Path) -> io::Result<()> {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let to = dest.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                copy_dir(&entry.path(), &to)?;
            } else {
                fs::copy(entry.path(), to)?;
            }
        }
        Ok(())
    }

    #[test]
    fn classifies_fast_forward_and_current() {
        let fixture = Fixture::new("ff");
        fixture.commit("root");
        fixture.mark_origin();
        assert_eq!(
            classify_history(&fixture.path).unwrap(),
            HistoryClass::Current
        );

        fs::write(
            fixture.path.join("src/main.rs"),
            "fn main() { /* ahead */ }\n",
        )
        .unwrap();
        fixture.commit("origin tip");
        let tip = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        run(&fixture.path, &["checkout", "-B", "main", "HEAD~1"]);
        run(
            &fixture.path,
            &["update-ref", "refs/remotes/origin/main", &tip],
        );
        assert_eq!(
            classify_history(&fixture.path).unwrap(),
            HistoryClass::FastForward
        );
    }

    #[test]
    fn unrelated_with_yes_moves_aside_and_clones_stand_in() {
        let old = Fixture::new("unrelated-old");
        old.commit("old root");
        fs::write(old.path.join("src/main.rs"), "fn main() { /* v072 */ }\n").unwrap();
        old.commit("Add recall update --verbose (0.7.2)");
        old.mark_origin();

        let stand_in = Fixture::new("stand-in");
        stand_in.commit("fresh main");
        stand_in.mark_origin();

        let old_path = old.path.clone();
        let stand_path = stand_in.path.clone();

        let fresh_url = format!("file://{}", stand_path.display());
        run(&old_path, &["remote", "add", "fresh", &fresh_url]);
        run(
            &old_path,
            &[
                "fetch",
                "--quiet",
                "fresh",
                "+main:refs/remotes/origin/main",
            ],
        );

        assert!(matches!(
            classify_history(&old_path).unwrap(),
            HistoryClass::Unrelated { .. }
        ));

        let parent = old_path.parent().unwrap().to_path_buf();
        let repair = aside_and_clone(&old_path, "2026-11-19", stand_in_clone(&stand_path)).unwrap();
        assert!(repair.aside.exists());
        assert!(repair.aside.starts_with(&parent));
        assert!(repair
            .aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("old-2026-11-19"));
        assert!(old_path.exists());
        assert!(old_path.join("Cargo.toml").is_file());
        assert!(repair
            .unique_subjects
            .iter()
            .any(|s| s.contains("Add recall update --verbose")));
        assert!(repair.aside.join("src/main.rs").is_file());
    }

    #[test]
    fn aside_name_collision_uses_dash_two() {
        let again = Fixture::new("collision-src");
        again.commit("c1");
        again.mark_origin();
        let aside1 = choose_aside_path(&again.path, "2026-11-19").unwrap();
        fs::create_dir_all(&aside1).unwrap();
        let aside2 = choose_aside_path(&again.path, "2026-11-19").unwrap();
        assert!(aside2
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-2"));
    }

    #[test]
    fn local_only_without_yes_on_non_tty_refuses() {
        let fixture = Fixture::new("local-only");
        fixture.commit("root");
        fixture.mark_origin();
        fs::write(fixture.path.join("note.txt"), "x\n").unwrap();
        fixture.commit("local tip");
        let class = classify_history(&fixture.path).unwrap();
        assert!(matches!(class, HistoryClass::LocalOnly { count: 1 }));
        let head_before = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        let err = maybe_repair_history(&fixture.path, &class, false, "2026-11-19", |_| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--yes"), "{err}");
        let head_after = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(head_before, head_after);
        assert!(!confirm_aside(false, false).unwrap());
        assert!(confirm_aside(true, false).unwrap());
    }

    #[test]
    fn local_only_with_yes_moves_and_keeps_subjects() {
        let fixture = Fixture::new("local-yes");
        fixture.commit("root");
        fixture.mark_origin();
        fs::write(fixture.path.join("a.txt"), "a\n").unwrap();
        fixture.commit("local one");
        fs::write(fixture.path.join("b.txt"), "b\n").unwrap();
        fixture.commit("local two");

        let stand_in = Fixture::new("local-stand");
        stand_in.commit("origin tip");
        stand_in.mark_origin();
        let stand_path = stand_in.path.clone();
        let origin_tip = git_output(&stand_path, &["rev-parse", "HEAD"]).unwrap();

        let class = classify_history(&fixture.path).unwrap();
        assert!(matches!(class, HistoryClass::LocalOnly { count: 2 }));
        let repair = maybe_repair_history(
            &fixture.path,
            &class,
            true,
            "2026-11-19",
            stand_in_clone(&stand_path),
        )
        .unwrap()
        .expect("repair ran");
        assert!(repair
            .unique_subjects
            .iter()
            .any(|s| s.contains("local one")));
        assert!(repair
            .unique_subjects
            .iter()
            .any(|s| s.contains("local two")));
        assert!(repair.aside.join("a.txt").is_file());
        assert!(repair.aside.join("b.txt").is_file());
        let new_head = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        // Stand-in copy keeps its HEAD; compare to the tip we cloned from.
        assert_eq!(new_head, origin_tip);
    }

    #[test]
    fn diverged_lists_only_local_unique_subjects() {
        let fixture = Fixture::new("diverged");
        fixture.commit("shared root");
        let shared = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        fs::write(fixture.path.join("local-only.txt"), "l\n").unwrap();
        fixture.commit("local only subject");
        run(&fixture.path, &["checkout", "-b", "origin-side", &shared]);
        fs::write(fixture.path.join("remote-only.txt"), "r\n").unwrap();
        fixture.commit("remote only subject");
        let remote_tip = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        run(&fixture.path, &["checkout", "main"]);
        run(
            &fixture.path,
            &["update-ref", "refs/remotes/origin/main", &remote_tip],
        );
        let class = classify_history(&fixture.path).unwrap();
        assert!(matches!(class, HistoryClass::Diverged { .. }), "{class:?}");
        let subjects = unique_local_subjects(&fixture.path).unwrap();
        assert!(
            subjects.iter().any(|s| s.contains("local only subject")),
            "{subjects:?}"
        );
        assert!(
            subjects.iter().all(|s| !s.contains("remote only subject")),
            "{subjects:?}"
        );
        let repair = aside_and_clone(&fixture.path, "2026-11-19", |path| {
            fs::create_dir_all(path.join("src"))?;
            fs::write(path.join("Cargo.toml"), "[package]\nname=\"recall\"\n")?;
            Ok(())
        })
        .unwrap();
        assert!(repair
            .unique_subjects
            .iter()
            .any(|s| s.contains("local only subject")));
    }

    #[test]
    fn dirty_checkout_never_needs_aside_until_clean() {
        let fixture = Fixture::new("dirty");
        fixture.commit("root");
        fixture.mark_origin();
        fs::write(fixture.path.join("dirty.txt"), "nope\n").unwrap();
        // Classification still runs on dirty trees; update() refuses earlier via ensure_clean.
        let class = classify_history(&fixture.path).unwrap();
        assert_eq!(class, HistoryClass::Current);
        assert!(!needs_aside_repair(&class));
        assert!(fixture.path.join("dirty.txt").is_file());
        let aside = choose_aside_path(&fixture.path, "2026-11-19").unwrap();
        assert!(!aside.exists());
    }

    #[test]
    fn prompt_helpers_match_spec_shape() {
        let checkout = PathBuf::from("/Users/matthew/Scripts/recall");
        let aside = PathBuf::from("/Users/matthew/Scripts/recall-old-2026-11-19");
        let unrelated = HistoryClass::Unrelated {
            ahead: 33,
            behind: 38,
        };
        let msg = prompt_message(&checkout, &unrelated, &aside);
        assert!(msg.contains("does not share history"));
        assert!(msg.contains("33 commits ahead and 38 commits behind"));
        assert!(msg.contains("[Y/n]"));
        let local = HistoryClass::LocalOnly { count: 2 };
        let msg = prompt_message(&checkout, &local, &aside);
        assert!(msg.contains("2 commits that are not on origin/main"));
        let non = noninteractive_message(&checkout, &unrelated);
        assert!(non.contains("--yes"));
        assert!(!non.contains("[Y/n]"));
    }

    #[test]
    fn repair_does_not_call_destructive_git() {
        // Guard production code only; fixtures may move HEAD for setup.
        let source = include_str!("update_repair.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        for banned in ["reset --hard", "git stash", "rebase", "rm -rf"] {
            assert!(
                !production.contains(banned),
                "repair path must not contain {banned}"
            );
        }
    }

    /// F1: stale `origin/main` on an unrelated remote must update via fetch so
    /// classification can reach Unrelated (aside prompt). Non-force refspec
    /// leaves the tracking tip stuck; production `fetch_origin_main` must not.
    #[test]
    fn unrelated_remote_fetch_updates_tracking_ref() {
        use crate::update::fetch_origin_main;

        let old = Fixture::new("stale-track");
        old.commit("old root");
        old.mark_origin();
        let head_before = git_output(&old.path, &["rev-parse", "HEAD"]).unwrap();
        let stale_origin =
            git_output(&old.path, &["rev-parse", "refs/remotes/origin/main"]).unwrap();

        let fresh = Fixture::new("fresh-remote");
        fresh.commit("fresh unrelated root");
        let fresh_tip = git_output(&fresh.path, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(head_before, fresh_tip);

        let url = format!("file://{}", fresh.path.display());
        run(&old.path, &["remote", "set-url", "origin", &url]);

        // Reproduce the hole: non-force refspec rejects the unrelated update.
        let non_force = git_at(&old.path)
            .args([
                "fetch",
                "--quiet",
                "origin",
                "refs/heads/main:refs/remotes/origin/main",
            ])
            .status()
            .unwrap();
        assert!(
            !non_force.success(),
            "non-force refspec should fail on unrelated remote"
        );
        assert_eq!(
            git_output(&old.path, &["rev-parse", "refs/remotes/origin/main"]).unwrap(),
            stale_origin,
            "failed non-force fetch must leave the stale tracking tip"
        );

        fetch_origin_main(&old.path).expect("force-tracking fetch must update origin/main");

        let head_after = git_output(&old.path, &["rev-parse", "HEAD"]).unwrap();
        let origin_after =
            git_output(&old.path, &["rev-parse", "refs/remotes/origin/main"]).unwrap();
        assert_eq!(head_after, head_before, "fetch must not move local HEAD");
        assert_eq!(
            origin_after, fresh_tip,
            "tracking tip must become fresh main"
        );
        assert_ne!(origin_after, stale_origin);
        assert!(
            matches!(
                classify_history(&old.path).unwrap(),
                HistoryClass::Unrelated { .. }
            ),
            "after fetch, history must classify as Unrelated"
        );
    }

    /// F2: a failed clone must leave the original checkout path in place.
    /// Clone into a sibling temp first so a partial clone cannot block undo
    /// and leave the user's tree only at an aside path.
    #[test]
    fn failed_clone_leaves_checkout_unmoved() {
        let fixture = Fixture::new("clone-fail");
        fixture.commit("keep me");
        fixture.mark_origin();
        let head = git_output(&fixture.path, &["rev-parse", "HEAD"]).unwrap();
        let checkout = fixture.path.clone();

        let err = aside_and_clone(&checkout, "2026-11-19", |dest| {
            // Occupy the clone destination (old code used the checkout path).
            fs::create_dir_all(dest)?;
            fs::write(dest.join("partial.txt"), "partial\n")?;
            Err(io::Error::other("clone boom"))
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("clone boom"), "{err}");
        assert!(checkout.is_dir(), "checkout path must still exist");
        assert!(
            !checkout.join("partial.txt").is_file(),
            "partial clone must not replace the user's checkout"
        );
        assert_eq!(
            git_output(&checkout, &["rev-parse", "HEAD"]).unwrap(),
            head,
            "local commits must remain at the original path"
        );
        let parent = checkout.parent().unwrap();
        let name = checkout.file_name().unwrap().to_string_lossy();
        for entry in fs::read_dir(parent).unwrap() {
            let entry = entry.unwrap();
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname.starts_with(&format!("{name}-old-")) {
                panic!("aside path left behind after failed clone: {fname}");
            }
            if fname.contains(".recall-clone-") {
                panic!("temp clone path left behind after failed clone: {fname}");
            }
        }
    }

    /// F4: Ctrl-D / EOF at `[Y/n]` must refuse (not treat empty buffer as yes).
    #[test]
    fn eof_at_prompt_refuses() {
        assert!(
            !accept_aside_answer(0, ""),
            "EOF (0 bytes) must refuse aside-and-clone"
        );
        assert!(accept_aside_answer(1, "\n"), "Enter alone remains yes");
        assert!(accept_aside_answer(2, "y\n"));
        assert!(accept_aside_answer(4, "yes\n"));
        assert!(!accept_aside_answer(2, "n\n"));
        assert!(!accept_aside_answer(3, "no\n"));
        assert!(!accept_aside_answer(6, "maybe\n"));
    }
}
