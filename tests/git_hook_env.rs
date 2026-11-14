include!("../git_hook_env.rs");

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn scrub_git_command_drops_a_leaked_hook_environment() {
    let dir = std::env::temp_dir().join(format!("recall-git-hook-env-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let init = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let leaked = git_with_hook_leak(&dir)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .unwrap();
    assert!(
        !leaked.status.success(),
        "a leaked GIT_DIR must make this fixture fail before the scrub"
    );

    let mut scrubbed = git_with_hook_leak(&dir);
    scrub_git_command(&mut scrubbed);
    let output = scrubbed
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "false");
    let _ = fs::remove_dir_all(&dir);
}

fn git_with_hook_leak(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_DIR", "/tmp/recall-not-a-git-dir")
        .env("GIT_INDEX_FILE", "/tmp/recall-not-an-index")
        .env("GIT_WORK_TREE", "/tmp")
        .env("GIT_OBJECT_DIRECTORY", "/tmp/recall-not-objects")
        .env("GIT_COMMON_DIR", "/tmp")
        .env("GIT_CONFIG_PARAMETERS", "'core.bare=true'")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.bare")
        .env("GIT_CONFIG_VALUE_0", "true");
    command
}
