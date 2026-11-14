/// Same names as scripts/git_env.py EXACT. build.rs cannot import Python.
pub const HOOK_GIT_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

pub const HOOK_GIT_PREFIXES: &[&str] = &["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// Drop hook-exported git variables from a child command.
pub fn scrub_git_command(command: &mut Command) {
    for name in HOOK_GIT_VARS {
        command.env_remove(*name);
    }
    for (key, _) in std::env::vars_os() {
        let text = key.to_string_lossy();
        if HOOK_GIT_PREFIXES
            .iter()
            .any(|prefix| text.starts_with(prefix))
        {
            command.env_remove(key);
        }
    }
}
