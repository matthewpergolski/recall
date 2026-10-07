use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn recall() -> Command {
    Command::new(env!("CARGO_BIN_EXE_recall"))
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
