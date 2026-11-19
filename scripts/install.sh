#!/usr/bin/env bash
# Install or update Recall on a Mac.
# Docs show: curl -fsSL …/install.sh | sh
# Non-interactive (no TTY): curl … | sh -s -- --yes
# Interactive curl | sh reads prompts from /dev/tty when available.
#
# Does not call an old `recall update`. Finds the checkout, repairs history when
# needed, builds the helper, and cargo-installs with --force.
set -euo pipefail

OFFICIAL_CLONE="https://github.com/matthewpergolski/recall.git"
YES=0
PLAN=0
REWRITE_CONFIG=""
REPO=""
DATE="$(date +%F)"
PENDING_CLONE=0
CONFIRM_USED_TTY=0

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [--yes] [--plan] [--repo <path>] [--rewrite-config <checkout>]

  --yes               Answer yes to tools and aside-and-clone prompts
  --plan              Print planned actions; change nothing
  --repo <path>       Use this checkout and write source_dir to it
  --rewrite-config <checkout>
                      Only patch top-level source_dir for that checkout, then exit

Non-interactive installs (curl | sh with no TTY) need --yes:
  curl -fsSL https://raw.githubusercontent.com/matthewpergolski/recall/main/scripts/install.sh | sh -s -- --yes
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --yes) YES=1; shift ;;
    --plan) PLAN=1; shift ;;
    --repo)
      REPO="${2:-}"
      [[ -n "$REPO" ]] || { echo "install.sh: --repo needs a path" >&2; exit 2; }
      shift 2
      ;;
    --rewrite-config)
      REWRITE_CONFIG="${2:-}"
      [[ -n "$REWRITE_CONFIG" ]] || { echo "install.sh: --rewrite-config needs a path" >&2; exit 2; }
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install.sh: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

log() { printf '%s\n' "$*"; }
plan() { printf 'plan: %s\n' "$*"; }

config_file() {
  if [[ -n "${XDG_CONFIG_HOME+x}" ]]; then
    printf '%s\n' "${XDG_CONFIG_HOME%/}/recall/config.toml"
  else
    printf '%s\n' "${HOME%/}/.config/recall/config.toml"
  fi
}

expand_path() {
  local value="$1"
  if [[ "$value" == ~/* ]]; then
    printf '%s\n' "${HOME%/}/${value#~/}"
  else
    printf '%s\n' "$value"
  fi
}

absolute_path() {
  local path="$1"
  if [[ -e "$path" ]] && command -v realpath >/dev/null 2>&1; then
    realpath "$path"
    return 0
  fi
  if [[ -d "$path" ]]; then
    (
      cd "$path"
      pwd -P
    )
    return 0
  fi
  # Path may not exist yet (plan-only clone target).
  local parent base
  parent="$(dirname "$path")"
  base="$(basename "$path")"
  if [[ -d "$parent" ]]; then
    printf '%s/%s\n' "$(cd "$parent" && pwd -P)" "$base"
  else
    printf '%s\n' "$path"
  fi
}

# Match src/update.rs normalized_remote: strip known prefixes, require exact owner/repo.
official_remote_ok() {
  local remote="$1"
  remote="${remote%"${remote##*[![:space:]]}"}"
  remote="${remote#"${remote%%[![:space:]]*}"}"
  remote="${remote%/}"
  while [[ "$remote" == *.git ]]; do
    remote="${remote%.git}"
  done
  local prefix
  for prefix in \
    "https://github.com/" \
    "http://github.com/" \
    "git@github.com:" \
    "ssh://git@github.com/"
  do
    case "$remote" in
      "$prefix"*) remote="${remote#"$prefix"}"; break ;;
    esac
  done
  local lower
  lower="$(printf '%s' "$remote" | tr '[:upper:]' '[:lower:]')"
  [[ "$lower" == "matthewpergolski/recall" ]]
}

looks_like_checkout_tree() {
  local path="$1"
  [[ -d "$path" \
    && -f "$path/Cargo.toml" \
    && -f "$path/capture-helper/Package.swift" ]]
}

is_checkout() {
  local path="$1"
  looks_like_checkout_tree "$path" || return 1
  local root remote
  root="$(git -C "$path" rev-parse --show-toplevel 2>/dev/null || true)"
  [[ -n "$root" ]] || return 1
  [[ "$(absolute_path "$root")" == "$(absolute_path "$path")" ]] || return 1
  remote="$(git -C "$path" config --get remote.origin.url 2>/dev/null || true)"
  official_remote_ok "$remote" || return 1
  return 0
}

# Patch only the top-level source_dir line. Mirror src/config.rs.
rewrite_source_dir() {
  local checkout="$1"
  local cfg file_dir abs quoted line
  cfg="$(config_file)"
  file_dir="$(dirname "$cfg")"
  mkdir -p "$file_dir"
  abs="$(absolute_path "$checkout")"
  quoted="$(printf '%s' "$abs" | sed 's/\\/\\\\/g; s/"/\\"/g')"
  line="source_dir = \"$quoted\""

  if [[ ! -f "$cfg" ]]; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "create $cfg with $line"
      return 0
    fi
    printf '%s\n' "$line" >"$cfg"
    return 0
  fi

  if [[ "$PLAN" -eq 1 ]]; then
    plan "patch top-level source_dir in $cfg to $abs"
    return 0
  fi

  python3 - "$cfg" "$line" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
new_line = sys.argv[2]
content = path.read_text(encoding="utf-8")
out = []
section = False
replaced = False
had_nl = content.endswith("\n")
for raw in content.splitlines(keepends=True):
    bare = raw.rstrip("\r\n")
    trimmed = bare.split("#", 1)[0].strip()
    if not section and trimmed.startswith("[") and trimmed.endswith("]"):
        if not replaced:
            if out and not "".join(out).endswith("\n"):
                out.append("\n")
            out.append(new_line + "\n")
            replaced = True
        section = True
    if not section:
        key = trimmed.split("=", 1)[0].strip() if "=" in trimmed else None
        if key == "source_dir":
            out.append(new_line + ("\n" if raw.endswith("\n") else ""))
            replaced = True
            continue
    out.append(raw)
body = "".join(out)
if not replaced:
    if body and not body.endswith("\n"):
        body += "\n"
    body += new_line + "\n"
elif had_nl and not body.endswith("\n"):
    body += "\n"
path.write_text(body, encoding="utf-8")
PY
}

read_configured_source_dir() {
  local cfg
  cfg="$(config_file)"
  [[ -f "$cfg" ]] || return 1
  python3 - "$cfg" <<'PY'
import pathlib, sys
section = False
for raw in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    trimmed = raw.split("#", 1)[0].strip()
    if trimmed.startswith("[") and trimmed.endswith("]"):
        section = True
        continue
    if section or "=" not in trimmed:
        continue
    key, value = trimmed.split("=", 1)
    if key.strip() != "source_dir":
        continue
    value = value.strip()
    if value.startswith('"') and value.endswith('"') and len(value) >= 2:
        value = value[1:-1]
    print(value)
    break
else:
    sys.exit(1)
PY
}

common_candidates() {
  local home="${HOME%/}"
  printf '%s\n' \
    "$home/Projects/recall" \
    "$home/Developer/recall" \
    "$home/src/recall" \
    "$home/Source/recall" \
    "$home/Code/recall" \
    "$home/code/recall" \
    "$home/Workspace/recall" \
    "$home/workspace/recall" \
    "$home/Documents/recall" \
    "$home/Documents/GitHub/recall" \
    "$home/Documents/Projects/recall"
}

find_checkout() {
  local path configured
  PENDING_CLONE=0
  if [[ -n "$REPO" ]]; then
    path="$(expand_path "$REPO")"
    if is_checkout "$path"; then
      CHECKOUT="$(absolute_path "$path")"
      WRITE_SOURCE_DIR=1
      return 0
    fi
    echo "install.sh: --repo is not a Recall checkout: $path" >&2
    return 1
  fi
  if [[ -n "${RECALL_REPO:-}" ]]; then
    path="$(expand_path "$RECALL_REPO")"
    if is_checkout "$path"; then
      CHECKOUT="$(absolute_path "$path")"
      WRITE_SOURCE_DIR=1
      return 0
    fi
    echo "install.sh: RECALL_REPO is not a Recall checkout: $path" >&2
    return 1
  fi
  if configured="$(read_configured_source_dir 2>/dev/null)"; then
    path="$(expand_path "$configured")"
    if is_checkout "$path"; then
      CHECKOUT="$(absolute_path "$path")"
      WRITE_SOURCE_DIR=0
      return 0
    fi
    # Pin exists as a checkout-shaped tree but failed the remote/root check: stop.
    if looks_like_checkout_tree "$path"; then
      local remote
      remote="$(git -C "$path" config --get remote.origin.url 2>/dev/null || echo "(none)")"
      echo "install.sh: source_dir $path is not the official Recall remote ($remote)." >&2
      echo "Fix origin, pass --repo <path>, or clear source_dir. Not cloning ~/Projects/recall over this pin." >&2
      return 1
    fi
    # Missing or not a tree — fall through and rewrite later.
    WRITE_SOURCE_DIR=1
  else
    WRITE_SOURCE_DIR=1
  fi

  local cwd candidate preferred
  cwd="$(pwd -P)"
  while [[ -n "$cwd" ]]; do
    if is_checkout "$cwd"; then
      CHECKOUT="$(absolute_path "$cwd")"
      return 0
    fi
    [[ "$cwd" == "/" ]] && break
    cwd="$(dirname "$cwd")"
  done

  preferred="${HOME%/}/Projects/recall"
  local -a found=()
  local candidate_list candidate
  # Avoid process substitution: macOS /bin/sh (bash 3.2 POSIX) rejects it.
  candidate_list="$(common_candidates)"
  while IFS= read -r candidate; do
    [[ -n "$candidate" ]] || continue
    if is_checkout "$candidate"; then
      found+=("$(absolute_path "$candidate")")
    fi
  done <<EOF
$candidate_list
EOF

  if is_checkout "$preferred"; then
    CHECKOUT="$(absolute_path "$preferred")"
    return 0
  fi
  if [[ ${#found[@]} -eq 1 ]]; then
    CHECKOUT="${found[0]}"
    return 0
  fi
  if [[ ${#found[@]} -gt 1 ]]; then
    echo "Multiple Recall checkouts were found:" >&2
    local item
    for item in "${found[@]}"; do
      echo "  - $item" >&2
    done
    echo "Choose one with --repo <path> or re-run the install command with --repo <path>." >&2
    return 1
  fi

  # No checkout: clone into canonical path unless occupied.
  if [[ -e "$preferred" ]]; then
    echo "install.sh: $preferred exists and is not a Recall checkout. Move it aside and re-run." >&2
    return 1
  fi
  if [[ "$PLAN" -eq 1 ]]; then
    plan "clone $OFFICIAL_CLONE into $preferred"
    CHECKOUT="$preferred"
    PENDING_CLONE=1
    return 0
  fi
  mkdir -p "$(dirname "$preferred")"
  git clone --quiet "$OFFICIAL_CLONE" "$preferred"
  CHECKOUT="$(absolute_path "$preferred")"
}

# Read yes/no from the controlling TTY when present (curl | sh). No TTY: refuse
# without printing a fake [Y/n] prompt. Caller supplies the non-interactive line.
confirm_tty() {
  local interactive_prompt="$1"
  local noninteractive_line="$2"
  CONFIRM_USED_TTY=0
  if [[ "$YES" -eq 1 ]]; then
    return 0
  fi
  # Prefer the controlling terminal (curl | sh). Open /dev/tty via exec so a
  # missing controlling terminal does not print "No such device" on stderr.
  local answer=""
  if exec 3<>/dev/tty 2>/dev/null; then
    CONFIRM_USED_TTY=1
    printf '%s ' "$interactive_prompt" >&3
    if IFS= read -r answer <&3; then
      exec 3>&-
      answer="$(printf '%s' "$answer" | tr '[:upper:]' '[:lower:]' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
      [[ -z "$answer" || "$answer" == y || "$answer" == yes ]]
      return $?
    fi
    exec 3>&-
    return 1
  fi
  echo "$noninteractive_line" >&2
  return 1
}

join_comma_space() {
  local -a items=("$@")
  local i out=""
  for ((i = 0; i < ${#items[@]}; i++)); do
    if [[ $i -gt 0 ]]; then
      out+=", "
    fi
    out+="${items[$i]}"
  done
  printf '%s\n' "$out"
}

tools_missing() {
  # Linux CI / unit tests skip the macOS tool prompt.
  if [[ "${RECALL_INSTALL_SKIP_TOOLS:-}" == "1" ]]; then
    return 1
  fi
  if [[ "$(uname -s)" != "Darwin" ]]; then
    return 1
  fi
  local -a missing=()
  if ! xcode-select -p >/dev/null 2>&1; then
    missing+=("Xcode Command Line Tools")
  fi
  if ! command -v rustc >/dev/null 2>&1 || ! rustup show active-toolchain >/dev/null 2>&1; then
    missing+=("rustup")
  fi
  if ! command -v ffmpeg >/dev/null 2>&1; then
    missing+=("ffmpeg")
  fi
  if [[ ${#missing[@]} -eq 0 ]]; then
    return 1
  fi
  TOOLS_MISSING_LINE="Missing: $(join_comma_space "${missing[@]}")"
  return 0
}

wait_for_xcode_clt() {
  local i
  for ((i = 0; i < 600; i++)); do
    if xcode-select -p >/dev/null 2>&1; then
      return 0
    fi
    # After the dialog has had time to appear, stop if no installer is running.
    # Dialog process is "Install Command Line Developer Tools".
    if [[ $i -gt 30 ]] && ! pgrep -f "Install Command Line Developer Tools" >/dev/null 2>&1; then
      return 1
    fi
    sleep 2
  done
  xcode-select -p >/dev/null 2>&1
}

rustc_meets_manifest() {
  local checkout="$1"
  local required have
  required="$(
    python3 - "$checkout/Cargo.toml" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
m = re.search(r'(?m)^rust-version\s*=\s*"([^"]+)"', text)
print(m.group(1) if m else "")
PY
  )"
  [[ -n "$required" ]] || return 0
  have="$(rustc --version 2>/dev/null | awk '{print $2}')"
  [[ -n "$have" ]] || return 1
  printf '%s\n%s\n' "$required" "$have" | sort -V | head -n1 | grep -qx "$required"
}

install_missing_tools() {
  if ! xcode-select -p >/dev/null 2>&1; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "install Xcode Command Line Tools via xcode-select --install (wait until ready)"
    else
      xcode-select --install || true
      log "Waiting for Xcode Command Line Tools…"
      if ! wait_for_xcode_clt; then
        echo "install.sh: Xcode Command Line Tools are still missing. Finish the dialog or install them, then re-run." >&2
        exit 1
      fi
    fi
  fi
  if ! command -v rustc >/dev/null 2>&1 || ! rustup show active-toolchain >/dev/null 2>&1; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "install rustup into ~/.cargo"
    else
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      # shellcheck disable=SC1091
      [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
    fi
  fi
  if ! command -v ffmpeg >/dev/null 2>&1; then
    if command -v brew >/dev/null 2>&1; then
      if [[ "$PLAN" -eq 1 ]]; then
        plan "brew install ffmpeg"
      else
        brew install ffmpeg
      fi
    else
      log "ffmpeg is missing and Homebrew is not installed. Install ffmpeg, place it at tools/ffmpeg/bin/ffmpeg, or set RECALL_FFMPEG_BIN=/path/to/ffmpeg, then re-run."
    fi
  fi
}

classify_history() {
  local checkout="$1"
  local head origin merge_base ahead behind
  head="$(git -C "$checkout" rev-parse HEAD)"
  if ! git -C "$checkout" rev-parse --verify refs/remotes/origin/main >/dev/null 2>&1; then
    HISTORY_CLASS="unknown"
    return 0
  fi
  origin="$(git -C "$checkout" rev-parse refs/remotes/origin/main)"
  if [[ "$head" == "$origin" ]]; then
    HISTORY_CLASS="current"
    return 0
  fi
  if ! merge_base="$(git -C "$checkout" merge-base HEAD refs/remotes/origin/main 2>/dev/null)"; then
    HISTORY_CLASS="unrelated"
    return 0
  fi
  if [[ "$merge_base" == "$head" ]]; then
    HISTORY_CLASS="fast-forward"
    return 0
  fi
  if [[ "$merge_base" == "$origin" ]]; then
    HISTORY_CLASS="local-only"
    return 0
  fi
  HISTORY_CLASS="diverged"
}

aside_path_for() {
  local checkout="$1"
  local parent name base candidate n
  parent="$(dirname "$checkout")"
  name="$(basename "$checkout")"
  base="$parent/${name}-old-${DATE}"
  if [[ ! -e "$base" ]]; then
    printf '%s\n' "$base"
    return 0
  fi
  n=2
  while [[ $n -lt 10000 ]]; do
    candidate="$parent/${name}-old-${DATE}-$n"
    if [[ ! -e "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    n=$((n + 1))
  done
  echo "install.sh: could not find a free aside path near $base" >&2
  return 1
}

choose_clone_temp() {
  local checkout="$1"
  local parent name pid n candidate
  parent="$(dirname "$checkout")"
  name="$(basename "$checkout")"
  pid="$$"
  for n in $(seq 0 9999); do
    candidate="$parent/.${name}.recall-clone-${pid}-${n}"
    if [[ ! -e "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "install.sh: could not find a free clone temp path near $parent" >&2
  return 1
}

print_unique_local_commits() {
  local checkout="$1"
  local aside="$2"
  local tmp subject omitted count
  tmp="$(mktemp "${TMPDIR:-/tmp}/recall-subjects.XXXXXX")"
  # Portable: no bash-4-only builtins, no process substitution.
  {
    git -C "$checkout" log --reverse --no-show-signature --format=%s HEAD --not refs/remotes/origin/main 2>/dev/null \
      || git -C "$checkout" log --reverse --no-show-signature --format=%s HEAD 2>/dev/null \
      || true
  } >"$tmp"
  log "Keeping local commits in $aside"
  count="$(wc -l <"$tmp" | tr -d '[:space:]')"
  if [[ -z "$count" || "$count" -eq 0 ]]; then
    rm -f "$tmp"
    return 0
  fi
  log "Unique local commits"
  omitted=0
  if [[ "$count" -gt 8 ]]; then
    omitted=$((count - 8))
    log "  and $omitted earlier"
  fi
  # --reverse log is oldest-first; tail shows the newest eight.
  tail -n 8 "$tmp" | while IFS= read -r subject; do
    [[ -n "$subject" ]] || continue
    subject="$(printf '%s' "$subject" | tr -d '\033')"
    if [[ ${#subject} -gt 96 ]]; then
      subject="${subject:0:93}..."
    fi
    log "  · $subject"
  done
  rm -f "$tmp"
}

aside_and_clone() {
  local checkout="$1"
  local aside temp
  aside="$(aside_path_for "$checkout")"
  if [[ "$PLAN" -eq 1 ]]; then
    plan "aside-and-clone: move $checkout to $aside and clone fresh into $checkout"
    return 0
  fi
  print_unique_local_commits "$checkout" "$aside"
  temp="$(choose_clone_temp "$checkout")"
  if ! git clone --quiet "$OFFICIAL_CLONE" "$temp"; then
    rm -rf "$temp"
    echo "install.sh: failed to clone $OFFICIAL_CLONE" >&2
    return 1
  fi
  if ! mv "$checkout" "$aside"; then
    rm -rf "$temp"
    echo "install.sh: failed to move $checkout aside" >&2
    return 1
  fi
  log "Moved $checkout to $aside"
  if ! mv "$temp" "$checkout"; then
    if mv "$aside" "$checkout"; then
      echo "install.sh: failed to move clone into $checkout; restored $aside back to $checkout" >&2
    else
      echo "install.sh: failed to move clone into $checkout; checkout is at $aside and clone temp may remain nearby" >&2
    fi
    return 1
  fi
  log "Cloned origin/main into $checkout"
}

ensure_clean_main() {
  local checkout="$1"
  local status branch upstream
  status="$(git -C "$checkout" status --porcelain --untracked-files=normal)"
  if [[ -n "$status" ]]; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "refuse dirty checkout (sample: $(printf '%s' "$status" | head -n1))"
      return 1
    fi
    echo "Recall checkout has uncommitted changes. Commit, discard, or move them before updating:" >&2
    printf '%s\n' "$status" >&2
    return 1
  fi
  branch="$(git -C "$checkout" branch --show-current)"
  if [[ "$branch" != "main" ]]; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "refuse branch '$branch' (require main)"
      return 1
    fi
    echo "Recall updates require the main branch; current branch is '$branch'." >&2
    return 1
  fi
  upstream="$(git -C "$checkout" rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)"
  if [[ "$upstream" != "origin/main" ]]; then
    if [[ "$PLAN" -eq 1 ]]; then
      plan "refuse upstream '${upstream:-none}' (require origin/main)"
      return 1
    fi
    echo "Recall main must track origin/main before updating; current upstream is '${upstream:-none}'." >&2
    return 1
  fi
  return 0
}

# --- rewrite-config only ---
if [[ -n "$REWRITE_CONFIG" ]]; then
  path="$(expand_path "$REWRITE_CONFIG")"
  if ! is_checkout "$path" && [[ "$PLAN" -eq 0 ]]; then
    # Allow rewrite against a path that looks like a checkout tree for tests.
    if [[ ! -d "$path" ]]; then
      echo "install.sh: --rewrite-config path does not exist: $path" >&2
      exit 1
    fi
  fi
  rewrite_source_dir "$path"
  exit 0
fi

CHECKOUT=""
WRITE_SOURCE_DIR=0
HISTORY_CLASS=""
TOOLS_MISSING_LINE=""

helper_binary_present() {
  local checkout="$1"
  local helper
  for helper in \
    "$checkout/capture-helper/.build/debug/recall-capture" \
    "$checkout/capture-helper/.build/arm64-apple-macosx/debug/recall-capture" \
    "$checkout/capture-helper/.build/x86_64-apple-macosx/debug/recall-capture"
  do
    if [[ -f "$helper" ]]; then
      return 0
    fi
  done
  return 1
}

cargo_bin_dir() {
  printf '%s\n' "${CARGO_HOME:-$HOME/.cargo}/bin"
}

# True when PATH's recall is Cargo's bin and embeds this checkout HEAD.
cargo_recall_matches_checkout() {
  local checkout="$1"
  local cargo_bin path head
  cargo_bin="$(cargo_bin_dir)/recall"
  path="$(command -v recall 2>/dev/null || true)"
  [[ -n "$path" && -x "$cargo_bin" ]] || return 1
  if command -v realpath >/dev/null 2>&1; then
    [[ "$(realpath "$path")" == "$(realpath "$cargo_bin")" ]] || return 1
  else
    [[ "$(absolute_path "$path")" == "$(absolute_path "$cargo_bin")" ]] || return 1
  fi
  head="$(git -C "$checkout" rev-parse HEAD 2>/dev/null || true)"
  [[ -n "$head" ]] || return 1
  # build.rs embeds RECALL_BUILD_COMMIT as ASCII in the binary.
  grep -a -F -q "$head" "$cargo_bin" 2>/dev/null
}

ensure_cargo_bin_on_path_files() {
  local bin_dir line rc
  bin_dir="$(cargo_bin_dir)"
  line="export PATH=\"$bin_dir:\$PATH\""
  for rc in "$HOME/.zprofile" "$HOME/.zshrc"; do
    if [[ -f "$rc" ]]; then
      if grep -Fq "$bin_dir" "$rc" 2>/dev/null; then
        continue
      fi
      if grep -Fq '.cargo/env' "$rc" 2>/dev/null; then
        continue
      fi
    fi
    printf '\n%s\n' "$line" >>"$rc"
  done
}

# Tools prompt (skipped entirely when nothing is missing).
if tools_missing; then
  if [[ "$PLAN" -eq 1 ]]; then
    plan "$TOOLS_MISSING_LINE. Install them"
  else
    if ! confirm_tty \
      "${TOOLS_MISSING_LINE}. Install them? [Y/n]" \
      "${TOOLS_MISSING_LINE}. Re-run with --yes to install them."
    then
      echo "$TOOLS_MISSING_LINE still missing." >&2
      exit 1
    fi
  fi
  install_missing_tools
fi

if ! find_checkout; then
  exit 1
fi

if [[ "$WRITE_SOURCE_DIR" -eq 1 ]]; then
  rewrite_source_dir "$CHECKOUT"
fi

# Plan-only pending clone: do not run git against the missing path.
if [[ "$PENDING_CLONE" -eq 1 ]]; then
  plan "fetch origin/main into $CHECKOUT after clone"
  plan "swift build --package-path $CHECKOUT/capture-helper"
  plan "cargo install --path $CHECKOUT --locked --force"
  plan "print PATH hits, doctor, sources, audio-tap-probe, and permission steps"
  log "plan: nothing else to change"
  exit 0
fi

if [[ "$PLAN" -eq 1 ]]; then
  if ! ensure_clean_main "$CHECKOUT"; then
    exit 0
  fi
else
  ensure_clean_main "$CHECKOUT"
fi

if [[ "$PLAN" -eq 1 ]]; then
  plan "fetch origin/main into $CHECKOUT"
elif [[ "${RECALL_INSTALL_SKIP_FETCH:-}" == "1" ]]; then
  :
else
  git -C "$CHECKOUT" fetch origin '+refs/heads/main:refs/remotes/origin/main'
fi

classify_history "$CHECKOUT"
case "$HISTORY_CLASS" in
  current)
    if [[ "$PLAN" -eq 1 ]]; then
      plan "already current at origin/main"
    fi
    ;;
  fast-forward)
    if [[ "$PLAN" -eq 1 ]]; then
      plan "fast-forward merge origin/main"
    else
      git -C "$CHECKOUT" merge --ff-only --quiet refs/remotes/origin/main
    fi
    ;;
  unrelated|diverged|local-only)
    if [[ "$PLAN" -eq 1 ]]; then
      plan "aside-and-clone for $HISTORY_CLASS history at $CHECKOUT"
    else
      ahead_behind="$(git -C "$CHECKOUT" rev-list --left-right --count HEAD...refs/remotes/origin/main)"
      ahead="${ahead_behind%%	*}"
      behind="${ahead_behind##*	}"
      aside="$(aside_path_for "$CHECKOUT")"
      if [[ "$HISTORY_CLASS" == "local-only" ]]; then
        interactive="Local main has ${ahead} commits that are not on origin/main.
Move $CHECKOUT to $aside and clone a fresh copy? [Y/n]"
        noninteractive="Local main has ${ahead} commits that are not on origin/main. Recall will not install commits that are only on the local main. Re-run with --yes to move $CHECKOUT aside and clone fresh."
      else
        interactive="Checkout $CHECKOUT does not share history with origin/main.
Local main is ${ahead} commits ahead and ${behind} commits behind origin/main.
Move it to $aside and clone a fresh copy into $CHECKOUT? [Y/n]"
        noninteractive="Checkout $CHECKOUT does not share history with origin/main. Local main is ${ahead} commits ahead and ${behind} commits behind origin/main. Re-run with --yes to move it aside and clone fresh, or pass --repo <path> to use another checkout."
      fi
      if ! confirm_tty "$interactive" "$noninteractive"; then
        if [[ "${CONFIRM_USED_TTY:-0}" -eq 1 ]]; then
          echo "Left $CHECKOUT unchanged. Re-run with --yes to move it aside and clone fresh, or pass --repo <path> to use another checkout." >&2
        fi
        exit 1
      fi
      aside_and_clone "$CHECKOUT"
    fi
    ;;
  *)
    if [[ "$PLAN" -eq 1 ]]; then
      plan "history class $HISTORY_CLASS; continue to build"
    fi
    ;;
esac

if [[ "$PLAN" -eq 1 ]]; then
  if [[ "$HISTORY_CLASS" == "current" ]] \
    && helper_binary_present "$CHECKOUT" \
    && cargo_recall_matches_checkout "$CHECKOUT"
  then
    log "plan: nothing else to change"
    exit 0
  fi
  if [[ "$HISTORY_CLASS" == "current" ]]; then
    if ! helper_binary_present "$CHECKOUT"; then
      plan "swift build --package-path $CHECKOUT/capture-helper (helper binary missing)"
    fi
    if ! cargo_recall_matches_checkout "$CHECKOUT"; then
      plan "cargo install --path $CHECKOUT --locked --force"
    fi
    log "plan: nothing else to change after those steps"
    exit 0
  fi
  plan "swift build --package-path $CHECKOUT/capture-helper"
  plan "cargo install --path $CHECKOUT --locked --force"
  plan "print PATH hits, doctor, sources, audio-tap-probe, and permission steps"
  log "plan: nothing else to change"
  exit 0
fi

# Real run: skip rebuild only when helper + PATH recall is Cargo's bin for HEAD.
RECALL_BIN="$(cargo_bin_dir)/recall"
if [[ "$HISTORY_CLASS" == "current" ]] \
  && helper_binary_present "$CHECKOUT" \
  && cargo_recall_matches_checkout "$CHECKOUT"
then
  log "Recall is already up to date at $CHECKOUT."
  exit 0
fi

# Cargo binary exists but is not on PATH: fix rc files for future shells, and
# put Cargo's bin on PATH for this process so a matching install can short-circuit.
if [[ -x "$RECALL_BIN" ]] && ! command -v recall >/dev/null 2>&1; then
  ensure_cargo_bin_on_path_files
  export PATH="$(cargo_bin_dir):${PATH:-}"
  if [[ "$HISTORY_CLASS" == "current" ]] \
    && helper_binary_present "$CHECKOUT" \
    && cargo_recall_matches_checkout "$CHECKOUT"
  then
    log "Recall is already up to date at $CHECKOUT."
    exit 0
  fi
fi

# Toolchain: update when rustup owns an old rustc; stop when rustc is too old
# and rustup does not own it.
if ! rustc_meets_manifest "$CHECKOUT"; then
  if command -v rustup >/dev/null 2>&1 && rustup show active-toolchain >/dev/null 2>&1; then
    rustup update stable
  else
    echo "install.sh: rustc is older than Cargo.toml rust-version and rustup does not own this toolchain. Install or update rustup, then re-run." >&2
    exit 1
  fi
fi

swift build --package-path "$CHECKOUT/capture-helper"
cargo install --path "$CHECKOUT" --locked --force

RECALL_BIN="$(cargo_bin_dir)/recall"
if ! command -v recall >/dev/null 2>&1 && [[ -x "$RECALL_BIN" ]]; then
  ensure_cargo_bin_on_path_files
fi

RECALL_HITS=()
IFS=':' read -r -a _path_parts <<<"${PATH:-}"
for _dir in "${_path_parts[@]}"; do
  [[ -z "$_dir" ]] && continue
  if [[ -f "$_dir/recall" && -x "$_dir/recall" ]]; then
    RECALL_HITS+=("$_dir/recall")
  fi
done
if [[ ${#RECALL_HITS[@]} -gt 1 ]]; then
  log "Several recall binaries are on PATH:"
  i=1
  for hit in "${RECALL_HITS[@]}"; do
    note=""
    [[ $i -eq 1 ]] && note=" (shell uses this one)"
    log "  $i. $hit$note"
    i=$((i + 1))
  done
  log "Remove the ones you do not want, or put Cargo's bin earlier on PATH."
fi

log "Grant Microphone and System Audio Recording Only to the terminal that launched this script,"
log "then quit that app and open it again. A new tab is not enough."
log "Next checks:"
log "  recall doctor"
log "  recall sources"
log "  recall audio-tap-probe"
log "Play system audio and watch the Call meter. Apple Speech needs an on-device model Recall does not download."
