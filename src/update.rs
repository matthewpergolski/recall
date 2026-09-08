use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::config::expand_path;

const RECALL_REMOTE: &str = "github.com/matthewpergolski/recall";
const BUILD_COMMIT: &str = env!("RECALL_BUILD_COMMIT");

#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub repo: Option<PathBuf>,
    pub configured_repo: Option<PathBuf>,
}

impl UpdateOptions {
    pub fn parse(args: Vec<String>, configured_repo: Option<PathBuf>) -> Result<Self, String> {
        let mut repo = None;
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--repo" => {
                    if repo.is_some() {
                        return Err("--repo may only be provided once".to_string());
                    }
                    repo = Some(expand_path(
                        iter.next()
                            .ok_or_else(|| "--repo requires a path".to_string())?,
                    ));
                }
                value => return Err(format!("Unknown update option: {value}")),
            }
        }

        Ok(Self {
            repo,
            configured_repo,
        })
    }
}

pub fn update(options: &UpdateOptions) -> io::Result<()> {
    let checkout = discover_checkout(options)?;
    validate_checkout(&checkout)?;
    ensure_clean(&checkout)?;
    ensure_update_branch(&checkout)?;

    let previous_commit = git_output(&checkout, &["rev-parse", "HEAD"])?;
    run_quiet_command(
        Command::new("git").arg("-C").arg(&checkout).args([
            "fetch",
            "--quiet",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
        ]),
        "fetch origin/main",
    )?;
    let remote_commit = git_output(&checkout, &["rev-parse", "refs/remotes/origin/main"])?;

    if previous_commit == remote_commit && build_matches(&previous_commit, BUILD_COMMIT) {
        println!("Already up to date ({}).", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if previous_commit != remote_commit {
        run_quiet_command(
            Command::new("git").arg("-C").arg(&checkout).args([
                "merge",
                "--ff-only",
                "--quiet",
                "refs/remotes/origin/main",
            ]),
            "fast-forward to origin/main",
        )?;
    }

    let crate_version = validate_checkout(&checkout)?;
    ensure_clean(&checkout)?;
    ensure_at_origin_main(&checkout)?;

    let current_commit = git_output(&checkout, &["rev-parse", "HEAD"])?;
    if previous_commit == current_commit {
        println!("Refreshing Recall {crate_version}...");
    } else {
        println!(
            "Updating Recall {} -> {}...",
            short_commit(&previous_commit),
            short_commit(&current_commit)
        );
    }

    run_quiet_step(
        "Tests",
        Command::new("cargo")
            .arg("test")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(checkout.join("Cargo.toml")),
    )?;
    run_quiet_step(
        "Capture helper",
        Command::new("swift")
            .arg("build")
            .arg("--package-path")
            .arg(checkout.join("capture-helper")),
    )?;
    ensure_clean(&checkout)?;
    ensure_at_origin_main(&checkout)?;
    run_quiet_step(
        "Install",
        Command::new("cargo")
            .arg("install")
            .arg("--path")
            .arg(&checkout)
            .arg("--locked")
            .arg("--force"),
    )?;

    if previous_commit == current_commit {
        println!("Recall refreshed ({crate_version}).");
    } else {
        println!(
            "Recall updated to {crate_version} ({}).",
            short_commit(&current_commit)
        );
    }

    Ok(())
}

fn discover_checkout(options: &UpdateOptions) -> io::Result<PathBuf> {
    if let Some(path) = &options.repo {
        return require_checkout(path, "--repo");
    }

    if let Some(path) = env::var_os("RECALL_REPO").map(PathBuf::from) {
        return require_checkout(
            &expand_path(path.to_string_lossy().into_owned()),
            "RECALL_REPO",
        );
    }

    if let Some(path) = &options.configured_repo {
        return require_checkout(path, "source_dir in Recall config");
    }

    let embedded = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if validate_checkout(&embedded).is_ok() {
        return canonical_path(&embedded);
    }

    if let Ok(current_dir) = env::current_dir() {
        for ancestor in current_dir.ancestors() {
            if validate_checkout(ancestor).is_ok() {
                return canonical_path(ancestor);
            }
        }
    }

    let valid_common = common_checkout_candidates()
        .into_iter()
        .filter(|path| validate_checkout(path).is_ok())
        .map(|path| canonical_path(&path))
        .collect::<io::Result<Vec<_>>>()?;
    let valid_common = deduplicate_paths(valid_common);

    match valid_common.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Could not find the Recall source checkout. Run `recall update --repo /path/to/recall`, set RECALL_REPO, or add source_dir to ~/.config/recall/config.toml.",
        )),
        paths => {
            let choices = paths
                .iter()
                .map(|path| format!("  - {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            Err(io::Error::other(format!(
                "Multiple Recall checkouts were found:\n{choices}\nChoose one with `recall update --repo <path>`."
            )))
        }
    }
}

fn require_checkout(path: &Path, source: &str) -> io::Result<PathBuf> {
    validate_checkout(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "The checkout from {source} is not valid ({}): {error}",
                path.display()
            ),
        )
    })?;
    canonical_path(path)
}

fn validate_checkout(path: &Path) -> io::Result<String> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "directory does not exist",
        ));
    }
    if !path.join("Cargo.toml").is_file() || !path.join("capture-helper/Package.swift").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Recall Cargo.toml or capture-helper/Package.swift is missing",
        ));
    }

    let root = git_output(path, &["rev-parse", "--show-toplevel"])?;
    if canonical_path(Path::new(&root))? != canonical_path(path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not the root of the Git checkout",
        ));
    }

    let remote = git_output(path, &["config", "--get", "remote.origin.url"])?;
    if normalized_remote(&remote) != Some(RECALL_REMOTE) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("origin is not the official Recall repository: {remote}"),
        ));
    }

    version_from_metadata(&cargo_metadata(path)?)
}

fn cargo_metadata(path: &Path) -> io::Result<serde_json::Value> {
    let metadata = command_output(
        Command::new("cargo")
            .arg("metadata")
            .arg("--no-deps")
            .arg("--format-version")
            .arg("1")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(path.join("Cargo.toml")),
        "read Cargo metadata",
    )?;
    serde_json::from_slice(&metadata.stdout).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Cargo metadata was not valid JSON: {error}"),
        )
    })
}

fn version_from_metadata(metadata: &serde_json::Value) -> io::Result<String> {
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|package| package["name"] == "recall"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cargo package is not named recall",
            )
        })?;
    let version = package["version"].as_str().unwrap_or("").trim();
    if version.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Recall package is missing a version",
        ));
    }
    Ok(version.to_string())
}

fn ensure_clean(checkout: &Path) -> io::Result<()> {
    let status = git_output(
        checkout,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    if status.is_empty() {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "Recall checkout has uncommitted changes. Commit, discard, or move them before updating:\n{status}"
    )))
}

fn ensure_update_branch(checkout: &Path) -> io::Result<()> {
    let branch = git_output(checkout, &["branch", "--show-current"])?;
    if branch != "main" {
        return Err(io::Error::other(format!(
            "Recall updates require the main branch; current branch is '{branch}'."
        )));
    }

    let upstream = git_output(
        checkout,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    if upstream != "origin/main" {
        return Err(io::Error::other(format!(
            "Recall main must track origin/main before updating; current upstream is '{upstream}'."
        )));
    }

    Ok(())
}

fn ensure_at_origin_main(checkout: &Path) -> io::Result<()> {
    let head = git_output(checkout, &["rev-parse", "HEAD"])?;
    let origin_main = git_output(checkout, &["rev-parse", "refs/remotes/origin/main"])?;
    if head == origin_main {
        return Ok(());
    }

    Err(io::Error::other(
        "Local main contains commits that are not on origin/main. Recall pulled safely but will not install unpublished code.",
    ))
}

fn git_output(checkout: &Path, args: &[&str]) -> io::Result<String> {
    let output = command_output(
        Command::new("git").arg("-C").arg(checkout).args(args),
        &format!("run git {}", args.join(" ")),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn command_output(command: &mut Command, description: &str) -> io::Result<Output> {
    let output = command.output().map_err(|error| {
        io::Error::new(error.kind(), format!("Failed to {description}: {error}"))
    })?;
    if output.status.success() {
        return Ok(output);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let details = match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("{stderr}\n{stdout}"),
        (false, true) => stderr,
        (true, false) => stdout,
        (true, true) => output.status.to_string(),
    };
    Err(io::Error::other(format!(
        "Failed to {description}: {details}"
    )))
}

fn run_quiet_command(command: &mut Command, description: &str) -> io::Result<()> {
    command_output(command, description).map(|_| ())
}

fn run_quiet_step(label: &str, command: &mut Command) -> io::Result<()> {
    print!("  {label}... ");
    io::stdout().flush()?;
    match run_quiet_command(command, label) {
        Ok(()) => {
            println!("done");
            Ok(())
        }
        Err(error) => {
            println!("failed");
            Err(error)
        }
    }
}

fn build_matches(source_commit: &str, build_commit: &str) -> bool {
    build_commit != "unknown" && source_commit == build_commit
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn canonical_path(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Could not resolve {}: {error}", path.display()),
        )
    })
}

fn common_checkout_candidates() -> Vec<PathBuf> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };

    [
        "Projects/recall",
        "Developer/recall",
        "src/recall",
        "Source/recall",
        "Code/recall",
        "code/recall",
        "Workspace/recall",
        "workspace/recall",
        "Documents/recall",
        "Documents/GitHub/recall",
        "Documents/Projects/recall",
    ]
    .into_iter()
    .map(|relative| home.join(relative))
    .collect()
}

fn deduplicate_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn normalized_remote(remote: &str) -> Option<&'static str> {
    let mut remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ] {
        if let Some(value) = remote.strip_prefix(prefix) {
            remote = value;
            break;
        }
    }

    if remote.eq_ignore_ascii_case("matthewpergolski/recall") {
        Some(RECALL_REMOTE)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_matches, deduplicate_paths, ensure_at_origin_main, ensure_clean, normalized_remote,
        short_commit, validate_checkout, version_from_metadata, UpdateOptions, RECALL_REMOTE,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    struct CheckoutFixture {
        path: PathBuf,
    }

    impl CheckoutFixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("recall-update-test-{}-{id}", std::process::id()));
            fs::create_dir_all(path.join("src")).unwrap();
            fs::create_dir_all(path.join("capture-helper")).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                "[package]\nname = \"recall\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
            fs::write(path.join("src/main.rs"), "fn main() {}\n").unwrap();
            fs::write(path.join("capture-helper/Package.swift"), "// fixture\n").unwrap();
            run_git(&path, &["init", "-b", "main"]);
            run_git(
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

        fn commit(&self) {
            run_git(&self.path, &["add", "."]);
            run_git(
                &self.path,
                &[
                    "-c",
                    "user.name=Recall Test",
                    "-c",
                    "user.email=recall@example.invalid",
                    "commit",
                    "-m",
                    "fixture",
                ],
            );
        }

        fn mark_origin_main(&self) {
            run_git(
                &self.path,
                &["update-ref", "refs/remotes/origin/main", "HEAD"],
            );
        }
    }

    impl Drop for CheckoutFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run_git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    #[test]
    fn parses_explicit_repo() {
        let options =
            UpdateOptions::parse(vec!["--repo".to_string(), "/tmp/recall".to_string()], None)
                .unwrap();

        assert_eq!(options.repo, Some(PathBuf::from("/tmp/recall")));
    }

    #[test]
    fn rejects_unknown_update_option() {
        let error = UpdateOptions::parse(vec!["--force".to_string()], None).unwrap_err();
        assert_eq!(error, "Unknown update option: --force");
    }

    #[test]
    fn recognizes_official_remote_forms() {
        for remote in [
            "https://github.com/matthewpergolski/recall.git",
            "git@github.com:matthewpergolski/recall.git",
            "ssh://git@github.com/matthewpergolski/recall",
        ] {
            assert_eq!(normalized_remote(remote), Some(RECALL_REMOTE));
        }
        assert_eq!(normalized_remote("https://example.com/recall.git"), None);
    }

    #[test]
    fn removes_duplicate_candidate_paths() {
        let paths = vec![
            PathBuf::from("/tmp/recall"),
            PathBuf::from("/tmp/other"),
            PathBuf::from("/tmp/recall"),
        ];

        assert_eq!(deduplicate_paths(paths).len(), 2);
    }

    #[test]
    fn skips_install_only_when_binary_and_source_commits_match() {
        let commit = "231cef6f6ff98638943ed5df694d3199c3ee07f1";

        assert!(build_matches(commit, commit));
        assert!(!build_matches(commit, "unknown"));
        assert!(!build_matches(commit, "different"));
    }

    #[test]
    fn formats_short_commits_without_assuming_the_input_length() {
        assert_eq!(short_commit("231cef6f6ff9"), "231cef6");
        assert_eq!(short_commit("abc"), "abc");
    }

    #[test]
    fn validates_an_official_recall_checkout() {
        let fixture = CheckoutFixture::new();

        assert_eq!(validate_checkout(&fixture.path).unwrap(), "0.0.0");
    }

    #[test]
    fn reads_the_recall_package_version_from_metadata() {
        let metadata = serde_json::json!({
            "packages": [
                {"name": "other", "version": "9.9.9"},
                {"name": "recall", "version": "0.3.0"}
            ]
        });

        assert_eq!(version_from_metadata(&metadata).unwrap(), "0.3.0");
    }

    #[test]
    fn rejects_metadata_without_a_recall_package() {
        let metadata = serde_json::json!({
            "packages": [{"name": "other", "version": "1.0.0"}]
        });
        let error = version_from_metadata(&metadata).unwrap_err();

        assert!(error.to_string().contains("not named recall"));
    }

    #[test]
    fn rejects_a_recall_package_with_a_blank_version() {
        let metadata = serde_json::json!({
            "packages": [{"name": "recall", "version": "  "}]
        });
        let error = version_from_metadata(&metadata).unwrap_err();

        assert!(error.to_string().contains("missing a version"));
    }

    #[test]
    fn refuses_a_checkout_with_uncommitted_files() {
        let fixture = CheckoutFixture::new();
        fixture.commit();
        assert!(ensure_clean(&fixture.path).is_ok());

        fs::write(fixture.path.join("local-change.txt"), "do not overwrite\n").unwrap();

        let error = ensure_clean(&fixture.path).unwrap_err();
        assert!(error.to_string().contains("uncommitted changes"));
        assert!(error.to_string().contains("local-change.txt"));
    }

    #[test]
    fn refuses_to_install_commits_not_on_origin_main() {
        let fixture = CheckoutFixture::new();
        fixture.commit();
        fixture.mark_origin_main();
        assert!(ensure_at_origin_main(&fixture.path).is_ok());

        fs::write(fixture.path.join("local-commit.txt"), "unpublished\n").unwrap();
        fixture.commit();

        let error = ensure_at_origin_main(&fixture.path).unwrap_err();
        assert!(error.to_string().contains("not on origin/main"));
    }
}
