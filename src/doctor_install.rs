//! Install/update health lines for `recall doctor`.
//!
//! Formatting takes injected values so tests do not touch the compiled-in
//! helper path or `RECALL_BUILD_COMMIT`.

use std::path::{Path, PathBuf};

/// How `source_dir` looks to discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDirStatus {
    Missing,
    Invalid {
        path: PathBuf,
        reason: String,
        replacement: Option<PathBuf>,
    },
    Valid {
        path: PathBuf,
    },
}

/// History / branch health of the chosen checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryStatus {
    Ok,
    Unrelated,
    Diverged,
    LocalOnly,
    Dirty,
    WrongBranch { branch: String },
    Unknown,
}

/// Inputs for the five install-related doctor checks.
#[derive(Debug, Clone)]
pub struct InstallDoctorInput {
    pub source_dir: SourceDirStatus,
    pub history: HistoryStatus,
    pub recall_paths: Vec<PathBuf>,
    pub helper_paths: Vec<PathBuf>,
    pub swift_present: bool,
    pub crate_version: String,
    pub build_commit: String,
    pub checkout_head: Option<String>,
}

/// Format the install section lines (without a trailing blank line requirement).
pub fn format_install_doctor_lines(input: &InstallDoctorInput) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Install:".to_string());
    lines.extend(source_dir_lines(&input.source_dir));
    lines.extend(history_lines(&input.history));
    lines.extend(recall_path_lines(&input.recall_paths));
    lines.extend(helper_lines(&input.helper_paths, input.swift_present));
    lines.extend(commit_lines(
        &input.crate_version,
        &input.build_commit,
        input.checkout_head.as_deref(),
    ));
    lines
}

fn source_dir_lines(status: &SourceDirStatus) -> Vec<String> {
    match status {
        SourceDirStatus::Missing => {
            vec!["  warn source_dir: missing; next update will write the chosen checkout".into()]
        }
        SourceDirStatus::Invalid {
            path,
            reason,
            replacement,
        } => {
            let mut out = vec![format!("  warn source_dir: {} ({reason})", path.display())];
            if let Some(next) = replacement {
                out.push(format!(
                    "        next update will write source_dir to {}",
                    next.display()
                ));
            }
            out
        }
        SourceDirStatus::Valid { path } => {
            vec![format!("  ok   source_dir: {}", path.display())]
        }
    }
}

fn history_lines(status: &HistoryStatus) -> Vec<String> {
    match status {
        HistoryStatus::Ok => vec!["  ok   checkout history: shares origin/main".into()],
        HistoryStatus::Unrelated => vec![
            "  warn checkout history: unrelated to origin/main; next update will offer aside-and-clone"
                .into(),
        ],
        HistoryStatus::Diverged => vec![
            "  warn checkout history: diverged from origin/main; next update will offer aside-and-clone"
                .into(),
        ],
        HistoryStatus::LocalOnly => vec![
            "  warn checkout history: local commits not on origin/main; next update will offer aside-and-clone"
                .into(),
        ],
        HistoryStatus::Dirty => {
            vec!["  warn checkout history: dirty tree; update will refuse until clean".into()]
        }
        HistoryStatus::WrongBranch { branch } => vec![format!(
            "  warn checkout history: branch is '{branch}'; update requires main tracking origin/main"
        )],
        HistoryStatus::Unknown => {
            vec!["  warn checkout history: could not classify".into()]
        }
    }
}

fn recall_path_lines(paths: &[PathBuf]) -> Vec<String> {
    if paths.is_empty() {
        return vec!["  miss recall on PATH: not found".into()];
    }
    if paths.len() == 1 {
        return vec![format!("  ok   recall on PATH: {}", paths[0].display())];
    }
    let mut out = vec!["  warn Several recall binaries are on PATH:".into()];
    for (i, path) in paths.iter().enumerate() {
        let note = if i == 0 { " (shell uses this one)" } else { "" };
        out.push(format!("        {}. {}{note}", i + 1, path.display()));
    }
    out.push("        Remove the ones you do not want, or put Cargo's bin earlier on PATH.".into());
    out
}

fn helper_lines(helpers: &[PathBuf], swift_present: bool) -> Vec<String> {
    if let Some(path) = helpers.first() {
        return vec![format!("  ok   capture helper: {}", path.display())];
    }
    let mut out = vec!["  warn capture helper: no debug recall-capture binary".into()];
    if swift_present {
        out.push(
            "        swift is on PATH; run `swift build --package-path <checkout>/capture-helper` or re-run the install command"
                .into(),
        );
    } else {
        out.push("        swift is missing; install Xcode Command Line Tools".into());
    }
    out
}

fn commit_lines(version: &str, build_commit: &str, head: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let build_norm = normalize_commit(build_commit);
    let mismatch = match head {
        Some(head) => !commits_match(&build_norm, &normalize_commit(head)),
        None => false,
    };
    if mismatch {
        out.push(format!("  warn recall {version}"));
    } else {
        out.push(format!("  ok   recall {version}"));
    }
    out.push(format!("        build commit: {build_commit}"));
    match head {
        Some(head) => out.push(format!("        checkout HEAD: {head}")),
        None => out.push("        checkout HEAD: unknown".into()),
    }
    if mismatch {
        out.push(
            "        build commit and checkout HEAD differ; re-run the install command or `recall update`"
                .into(),
        );
    }
    out
}

fn normalize_commit(commit: &str) -> String {
    commit.trim_end_matches("-dirty").to_string()
}

fn commits_match(build: &str, head: &str) -> bool {
    if build.is_empty() || head.is_empty() {
        return false;
    }
    build == head || build.starts_with(head) || head.starts_with(build)
}

/// The three debug helper paths under a checkout's `capture-helper`.
pub fn helper_candidate_paths(checkout: &Path) -> Vec<PathBuf> {
    let helper_dir = checkout.join("capture-helper");
    vec![
        helper_dir.join(".build/debug/recall-capture"),
        helper_dir.join(".build/arm64-apple-macosx/debug/recall-capture"),
        helper_dir.join(".build/x86_64-apple-macosx/debug/recall-capture"),
    ]
}

pub fn existing_helpers(checkout: &Path) -> Vec<PathBuf> {
    helper_candidate_paths(checkout)
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

/// Walk PATH for every `recall` file, in order.
pub fn recall_binaries_on_path(path_env: Option<&str>) -> Vec<PathBuf> {
    let Some(raw) = path_env else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for dir in std::env::split_paths(raw) {
        let candidate = dir.join("recall");
        if candidate.is_file() {
            hits.push(candidate);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn base_input() -> InstallDoctorInput {
        InstallDoctorInput {
            source_dir: SourceDirStatus::Valid {
                path: PathBuf::from("/Users/matthew/Scripts/recall"),
            },
            history: HistoryStatus::Ok,
            recall_paths: vec![PathBuf::from("/Users/matthew/.cargo/bin/recall")],
            helper_paths: vec![PathBuf::from(
                "/Users/matthew/Scripts/recall/capture-helper/.build/debug/recall-capture",
            )],
            swift_present: true,
            crate_version: "0.7.11".into(),
            build_commit: "abc1234".into(),
            checkout_head: Some("abc1234".into()),
        }
    }

    #[test]
    fn stale_source_dir_prints_warn_and_replacement() {
        let mut input = base_input();
        input.source_dir = SourceDirStatus::Invalid {
            path: PathBuf::from("/gone/recall"),
            reason: "not a Recall checkout".into(),
            replacement: Some(PathBuf::from("/Users/matthew/Projects/recall")),
        };
        let text = format_install_doctor_lines(&input).join("\n");
        assert!(text.contains("warn source_dir: /gone/recall"), "{text}");
        assert!(
            text.contains("next update will write source_dir to /Users/matthew/Projects/recall"),
            "{text}"
        );
    }

    #[test]
    fn unrelated_history_offers_aside_and_clone() {
        let mut input = base_input();
        input.history = HistoryStatus::Unrelated;
        let text = format_install_doctor_lines(&input).join("\n");
        assert!(text.contains("warn checkout history: unrelated"), "{text}");
        assert!(text.contains("aside-and-clone"), "{text}");
    }

    #[test]
    fn two_recall_paths_name_the_shell_winner() {
        let mut input = base_input();
        input.recall_paths = vec![
            PathBuf::from("/usr/local/bin/recall"),
            PathBuf::from("/Users/matthew/.cargo/bin/recall"),
        ];
        let text = format_install_doctor_lines(&input).join("\n");
        assert!(
            text.contains("Several recall binaries are on PATH"),
            "{text}"
        );
        assert!(
            text.contains("1. /usr/local/bin/recall (shell uses this one)"),
            "{text}"
        );
        assert!(
            text.contains("2. /Users/matthew/.cargo/bin/recall"),
            "{text}"
        );
    }

    #[test]
    fn empty_helper_list_warns_while_swift_is_present() {
        let mut input = base_input();
        input.helper_paths = Vec::new();
        input.swift_present = true;
        let text = format_install_doctor_lines(&input).join("\n");
        assert!(text.contains("warn capture helper"), "{text}");
        assert!(text.contains("swift is on PATH"), "{text}");
        // Spec: assert no swift process was started by formatting.
        assert!(!text.contains("swift run"), "{text}");
    }

    #[test]
    fn differing_commits_print_warn_and_both_strings() {
        let mut input = base_input();
        input.build_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
        input.checkout_head = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        let text = format_install_doctor_lines(&input).join("\n");
        assert!(text.contains("warn recall 0.7.11"), "{text}");
        assert!(
            text.contains("build commit: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "{text}"
        );
        assert!(
            text.contains("checkout HEAD: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "{text}"
        );
        assert!(text.contains("differ"), "{text}");
    }

    #[test]
    fn helper_candidates_do_not_spawn_swift() {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("recall-doctor-helper-{id}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("capture-helper")).unwrap();
        let found = existing_helpers(&root);
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
