use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Recall with its list of session places pointed at a file that is never
/// made, so a test neither reads nor writes the user's real list.
fn recall() -> Command {
    recall_with_places(&unique_storage("no-places").join("session-places.txt"))
}

fn recall_with_places(list: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recall"));
    command.env("RECALL_SESSION_PLACES", list);
    command
}

/// A folder that is not under the system temp folder. Recall keeps temp
/// folders off its list, so a test of the list needs one that is not. The
/// build folder serves, unless the checkout itself is under temp: then there
/// is no such folder to use and the test is skipped.
fn unique_place(label: &str) -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let resolved = fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    let under_temp = [
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
    ]
    .iter()
    .any(|root| resolved.starts_with(root))
        || resolved.starts_with(&temp);
    if under_temp {
        eprintln!("skipped {label}: the build folder is under the system temp folder");
        return None;
    }
    Some(base.join(format!(
        "recall-place-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )))
}

fn make_session(storage: &std::path::Path, folder: &str, created_at: u64) {
    let session = storage.join(folder);
    fs::create_dir_all(session.join(".recall")).unwrap();
    fs::write(
        session.join(".recall/metadata.json"),
        format!(
            r#"{{"created_at_unix": {created_at}, "title": "T", "consent": {{"mode": "noted"}}}}"#
        ),
    )
    .unwrap();
}

fn unique_storage(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "recall-cli-smoke-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ))
}

fn output_text(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}{stderr}")
}

#[test]
fn help_mentions_resume() {
    let output = recall()
        .arg("--help")
        .output()
        .expect("failed to run recall --help");
    assert!(output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(text.contains("recall --resume"));
    assert!(text.contains("--generation"));
}

#[test]
fn resume_latest_in_empty_storage_errors_without_opening_the_tui() {
    let storage = unique_storage("empty");
    fs::create_dir_all(&storage).unwrap();
    let output = recall()
        .arg("--storage")
        .arg(&storage)
        .arg("--resume")
        .arg("latest")
        .output()
        .expect("failed to run recall --resume latest");
    assert!(!output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(
        text.contains("Recall could not resume"),
        "unexpected output: {text}"
    );
    assert!(
        text.contains("No Recall sessions found"),
        "unexpected output: {text}"
    );
    let created = fs::read_dir(&storage)
        .unwrap()
        .filter_map(Result::ok)
        .count();
    assert_eq!(created, 0, "resume must not create a new session folder");
    let _ = fs::remove_dir_all(storage);
}

#[test]
fn resume_missing_session_id_errors_clearly() {
    let storage = unique_storage("missing-id");
    fs::create_dir_all(&storage).unwrap();
    let output = recall()
        .arg("--storage")
        .arg(&storage)
        .arg("--resume")
        .arg("2026-05-26_1921-et-does-not-exist")
        .output()
        .expect("failed to run recall --resume missing");
    assert!(!output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(
        text.contains("No Recall session matching"),
        "unexpected output: {text}"
    );
    let _ = fs::remove_dir_all(storage);
}

#[test]
fn resume_finds_a_session_by_part_of_its_name_and_reports_ambiguity() {
    let storage = unique_storage("partial-name");
    let folders = [
        "797394737678-2026-05-26_1921-et-design-sync",
        "797394737600-2026-05-26_2039-et-design-review",
    ];
    for folder in folders {
        let session = storage.join(folder);
        fs::create_dir_all(session.join(".recall/state")).unwrap();
        fs::write(
            session.join(".recall/metadata.json"),
            r#"{"created_at_unix": 1, "title": "Design", "consent": {"mode": "noted"}}"#,
        )
        .unwrap();
        // An unfinished take makes resume stop before the TUI, after the lookup.
        fs::write(
            session.join(".recall/state/capture.json"),
            r#"{"take_count": 1, "completed_take": 0, "elapsed_ms": 1000}"#,
        )
        .unwrap();
    }
    let resume = |target: &str| {
        let output = recall()
            .arg("--storage")
            .arg(&storage)
            .arg("--resume")
            .arg(target)
            .output()
            .expect("failed to run recall --resume by partial name");
        assert!(!output.status.success(), "{}", output_text(&output));
        output_text(&output)
    };

    for target in ["2026-05-26_1921-et-design-sync", "design-sync", "review"] {
        let text = resume(target);
        assert!(text.contains("unfinished take"), "{target}: {text}");
    }
    let text = resume("design");
    assert!(text.contains("matches 2 Recall sessions"), "{text}");
    assert!(text.contains("2026-05-26_1921-et-design-sync"), "{text}");
    assert!(text.contains("2026-05-26_2039-et-design-review"), "{text}");
    let _ = fs::remove_dir_all(storage);
}

#[test]
fn export_finds_a_session_by_part_of_its_name() {
    let storage = unique_storage("export-by-name");
    let folder = "797394737678-2026-05-26_1921-et-design-sync";
    let session = storage.join(folder);
    fs::create_dir_all(session.join(".recall")).unwrap();
    fs::write(
        session.join(".recall/metadata.json"),
        r#"{"created_at_unix": 1, "title": "Design"}"#,
    )
    .unwrap();
    let export = |target: &str| {
        let output = recall()
            .args(["export", target, "--storage"])
            .arg(&storage)
            .output()
            .expect("failed to run recall export by name");
        assert!(!output.status.success(), "{}", output_text(&output));
        output_text(&output)
    };

    // No meeting document yet, so export stops after the lookup and names the folder it found.
    for target in ["2026-05-26_1921-et-design-sync", "design-sync"] {
        let text = export(target);
        assert!(
            text.contains("Missing meeting document"),
            "{target}: {text}"
        );
        assert!(text.contains(folder), "{target}: {text}");
    }
    let text = export("standup");
    assert!(text.contains("No Recall session matching"), "{text}");
    let _ = fs::remove_dir_all(storage);
}

#[test]
fn resume_refuses_an_unfinished_take_from_the_cli() {
    let storage = unique_storage("unfinished");
    let session = storage.join("2026-05-26_1921-et-unfinished");
    fs::create_dir_all(session.join(".recall/state")).unwrap();
    fs::write(
        session.join(".recall/metadata.json"),
        r#"{"created_at_unix": 1, "title": "Unfinished", "consent": {"mode": "noted"}}"#,
    )
    .unwrap();
    fs::write(
        session.join(".recall/state/capture.json"),
        r#"{"take_count": 1, "completed_take": 0, "elapsed_ms": 1000}"#,
    )
    .unwrap();

    let output = recall()
        .arg("--storage")
        .arg(&storage)
        .arg("--resume")
        .arg("2026-05-26_1921-et-unfinished")
        .output()
        .expect("failed to run recall --resume unfinished");
    assert!(!output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(
        text.contains("unfinished take"),
        "unexpected output: {text}"
    );
    assert!(
        session.exists(),
        "refusing resume must not delete the session"
    );
    let _ = fs::remove_dir_all(storage);
}

#[test]
fn resume_subcommand_list_explains_how_to_pick() {
    let output = recall()
        .args(["resume", "--list"])
        .output()
        .expect("failed to run recall resume --list");
    assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(text.contains("recall list"), "unexpected output: {text}");
    assert!(
        text.contains("recall --resume"),
        "unexpected output: {text}"
    );
}

#[test]
fn transcribe_rejects_generation_zero() {
    let output = recall()
        .args(["transcribe", "latest", "--generation", "0"])
        .output()
        .expect("failed to run recall transcribe --generation 0");
    assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(
        text.contains("--generation must be greater than zero"),
        "unexpected output: {text}"
    );
}

#[test]
fn a_new_session_puts_its_folder_on_the_list_once() {
    let Some(root) = unique_place("start") else {
        return;
    };
    let storage = root.join("sessions");
    let list = root.join("config/session-places.txt");
    for title in ["First", "Second"] {
        let output = recall_with_places(&list)
            .args(["start", "--title", title, "--storage"])
            .arg(&storage)
            .output()
            .expect("failed to run recall start");
        assert!(output.status.success(), "{}", output_text(&output));
    }
    let listed = fs::read_to_string(&list).unwrap();
    let place = fs::canonicalize(&storage).unwrap();
    assert_eq!(listed, format!("{}\n", place.display()));

    // A temp folder stays off the list.
    let temp = unique_storage("start-temp");
    let output = recall_with_places(&list)
        .args(["start", "--title", "Smoke", "--storage"])
        .arg(&temp)
        .output()
        .expect("failed to run recall start");
    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(fs::read_to_string(&list).unwrap(), listed);

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(temp);
}

#[test]
fn a_list_that_cannot_be_written_warns_once_and_the_session_still_starts() {
    let Some(root) = unique_place("unwritable") else {
        return;
    };
    let storage = root.join("sessions");
    fs::create_dir_all(&root).unwrap();
    // A file where the list's folder should be.
    fs::write(root.join("config"), "not a folder").unwrap();
    let output = recall_with_places(&root.join("config/session-places.txt"))
        .args(["start", "--title", "Still works", "--storage"])
        .arg(&storage)
        .output()
        .expect("failed to run recall start");
    assert!(output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    assert!(text.contains("Recall session initialized"), "{text}");
    assert_eq!(
        text.matches("could not update its list of session places")
            .count(),
        1,
        "{text}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn list_all_and_places_cover_every_folder_on_the_list() {
    let Some(root) = unique_place("all") else {
        return;
    };
    let here = root.join("here/sessions");
    let work = root.join("work/sessions");
    let gone = root.join("gone/sessions");
    let list = root.join("session-places.txt");
    make_session(&here, "2026-06-01_0900-et-standup", 300);
    make_session(&work, "2026-06-02_0900-et-budget", 200);
    make_session(&work, "2026-06-03_0900-et-budget-review", 400);
    fs::create_dir_all(root.join("empty")).unwrap();
    let here = fs::canonicalize(&here).unwrap();
    let work = fs::canonicalize(&work).unwrap();
    fs::write(&list, format!("{}\n{}\n", work.display(), gone.display())).unwrap();

    // Plain list: this folder only. It also puts this folder on the list.
    let output = recall_with_places(&list)
        .args(["list", "--storage"])
        .arg(&here)
        .output()
        .expect("failed to run recall list");
    let text = output_text(&output);
    assert!(
        text.contains("standup") && !text.contains("budget"),
        "{text}"
    );
    assert!(fs::read_to_string(&list)
        .unwrap()
        .contains(&here.display().to_string()));

    // --all: this folder first, then the others, newest first in each.
    let output = recall_with_places(&list)
        .args(["list", "--all", "--storage"])
        .arg(&here)
        .output()
        .expect("failed to run recall list --all");
    assert!(output.status.success(), "{}", output_text(&output));
    let text = output_text(&output);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("no {needle} in {text}"))
    };
    assert!(at("standup") < at("budget-review"), "{text}");
    assert!(at("budget-review") < at("et-budget\n"), "{text}");
    assert!(
        text.contains("1 place(s) on the list are missing"),
        "{text}"
    );

    // From a folder with no sessions, --all still shows the rest.
    let output = recall_with_places(&list)
        .args(["list", "--all", "--storage"])
        .arg(root.join("empty/sessions"))
        .output()
        .expect("failed to run recall list --all");
    let text = output_text(&output);
    assert!(
        text.contains("standup") && text.contains("budget"),
        "{text}"
    );

    let output = recall_with_places(&list)
        .arg("places")
        .output()
        .expect("failed to run recall places");
    let text = output_text(&output);
    assert!(
        text.contains(&format!("2 sessions  {}", work.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("1 session  {}", here.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("missing  {}", gone.display())),
        "{text}"
    );

    // Forget drops the line and deletes nothing.
    let output = recall_with_places(&list)
        .args(["places", "forget"])
        .arg(&work)
        .output()
        .expect("failed to run recall places forget");
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_text(&output).contains("No session was deleted"));
    assert!(work.join("2026-06-02_0900-et-budget").is_dir());
    assert!(!fs::read_to_string(&list)
        .unwrap()
        .contains(&work.display().to_string()));
    let output = recall_with_places(&list)
        .args(["places", "forget"])
        .arg(&work)
        .output()
        .expect("failed to run recall places forget");
    assert_eq!(output.status.code(), Some(1), "{}", output_text(&output));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn export_finds_a_name_in_another_place_and_says_where() {
    let Some(root) = unique_place("elsewhere") else {
        return;
    };
    let here = root.join("here/sessions");
    let work = root.join("work/sessions");
    let list = root.join("session-places.txt");
    make_session(&here, "2026-06-01_0900-et-standup", 300);
    make_session(&work, "2026-06-02_0900-et-budget", 200);
    let work = fs::canonicalize(&work).unwrap();
    let session = work.join("2026-06-02_0900-et-budget");
    fs::write(session.join("meeting.md"), "# Budget\n").unwrap();
    fs::write(session.join("transcript.md"), "# Transcript\n\nhello\n").unwrap();
    fs::write(&list, format!("{}\n", work.display())).unwrap();

    let export = |list: &std::path::Path| {
        recall_with_places(list)
            .args(["export", "budget", "--storage"])
            .arg(&here)
            .output()
            .expect("failed to run recall export")
    };
    let output = export(&list);
    assert!(output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains(&format!("Found in {}", work.display())),
        "{}",
        output_text(&output)
    );
    assert!(work
        .join("2026-06-02_0900-et-budget/meeting-export.md")
        .exists());

    // Without the list the name is not found: the lookup stays in this folder.
    let output = export(&root.join("no-list.txt"));
    assert!(!output.status.success());
    assert!(output_text(&output).contains("No Recall session matching 'budget'"));

    let _ = fs::remove_dir_all(root);
}
