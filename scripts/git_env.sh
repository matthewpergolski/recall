# Sourced by the git hooks and scripts/check.sh.
# The variable names live in scripts/git_env.py. This file does not repeat them.
_git_env_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
eval "$(python3 "${_git_env_dir}/git_env.py" --unset)"
unset _git_env_dir
