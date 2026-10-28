#!/usr/bin/env bash
# Prove optional_cairnkit_check.sh (the path check.sh uses) stays green when:
#   1. cairnkit is on PATH but this checkout has no memory folder
#   2. legacy in-repo memory-bank/ exists without cairnkit memory setup
# PATH stubs avoid depending on a reviewer's real cairnkit config.
#
# Never create or delete ${repo}/memory-bank. Legacy case uses an isolated
# temp workdir so a real checkout memory-bank/ is left untouched.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
gate="${root}/scripts/optional_cairnkit_check.sh"
stub_root="$(mktemp -d)"
work="$(mktemp -d)"
log="${stub_root}/calls.log"
trap 'rm -rf "${stub_root}" "${work}"' EXIT

mkdir -p "${stub_root}/bin"
: >"${log}"

cat >"${stub_root}/bin/cairnkit" <<EOF
#!/usr/bin/env bash
printf 'cairnkit %s\n' "\$*" >>"${log}"
# Real CLI rejects \`cairnkit memory check\`; only \`cairnkit check\` is valid here.
if [[ "\$#" -eq 1 && "\$1" == "check" ]]; then
  exit 0
fi
echo "invalid choice: '\$1'" >&2
exit 2
EOF

cat >"${stub_root}/bin/memory" <<EOF
#!/usr/bin/env bash
printf 'memory %s\n' "\$*" >>"${log}"
# No memory folder for this checkout → status fails (and check would too).
exit 1
EOF

chmod +x "${stub_root}/bin/cairnkit" "${stub_root}/bin/memory"
export PATH="${stub_root}/bin:${PATH}"

run_case() {
  local label="$1"
  local cwd="$2"
  : >"${log}"
  if ! (cd "${cwd}" && bash "${gate}" >/dev/null 2>&1); then
    echo "FAIL: ${label}: optional_cairnkit_check.sh exited non-zero" >&2
    exit 1
  fi
  if grep -Eq '^cairnkit ' "${log}"; then
    echo "FAIL: ${label}: cairnkit was invoked (should skip)" >&2
    cat "${log}" >&2
    exit 1
  fi
  if ! grep -Eq '^memory status$' "${log}"; then
    echo "FAIL: ${label}: expected memory status probe" >&2
    cat "${log}" >&2
    exit 1
  fi
}

# Case 1: cairnkit installed, no memory folder for this checkout.
run_case "no-memory-folder" "${root}"

# Case 2: legacy memory-bank/ only — in an isolated temp workdir, not ${root}.
mkdir -p "${work}/memory-bank"
printf 'legacy-only\n' >"${work}/memory-bank/note.md"
run_case "legacy-memory-bank-only" "${work}"

# Positive control: when memory status succeeds, cairnkit check must run.
cat >"${stub_root}/bin/memory" <<EOF
#!/usr/bin/env bash
printf 'memory %s\n' "\$*" >>"${log}"
exit 0
EOF
chmod +x "${stub_root}/bin/memory"
: >"${log}"
if ! (cd "${root}" && bash "${gate}" >/dev/null 2>&1); then
  echo "FAIL: configured memory: optional_cairnkit_check.sh exited non-zero" >&2
  exit 1
fi
if ! grep -Eq '^cairnkit check$' "${log}"; then
  echo "FAIL: configured memory: expected cairnkit check" >&2
  cat "${log}" >&2
  exit 1
fi
if grep -Eq '^cairnkit memory' "${log}"; then
  echo "FAIL: configured memory: used invalid cairnkit memory …" >&2
  cat "${log}" >&2
  exit 1
fi

echo "cairnkit check gate self-check passed."
