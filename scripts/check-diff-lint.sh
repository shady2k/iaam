#!/usr/bin/env bash
# Guard against weakening checks in the diff (§15.7).
# When faced with a failing lint, an agent may add an allow instead of fixing the issue.
set -euo pipefail

# Determine the root from the script directory, not the caller's cwd: otherwise,
# running from a non-git directory yields an empty string and `cd ""`, causing the
# guard to check the wrong directory. If the root cannot be determined, refuse to proceed.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "DIFF-LINT: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

BASE="${1:-}"

if [ -z "$BASE" ]; then
  echo "ERROR: comparison base was not provided." >&2
  echo "A guard that silently skips itself when the base is absent is useless:" >&2
  echo "that is exactly the state in which weakened checks would pass through it." >&2
  exit 1
fi

# The base may be a commit (the usual case) or a tree: on the first push to a
# branch, CI supplies the hash of the empty tree. The diff forms are DIFFERENT.
# `git diff <tree>...HEAD` fails with the fatal error “is a tree, not a commit”,
# and with `|| true` on the pipeline it would be interpreted as “no violations”.
# Therefore, resolve the base explicitly and invoke `git diff` without masking its status.
if BASE_RESOLVED=$(git rev-parse --verify --quiet "${BASE}^{commit}"); then
  DIFF_RANGE=("${BASE_RESOLVED}...HEAD")
elif BASE_RESOLVED=$(git rev-parse --verify --quiet "${BASE}^{tree}"); then
  DIFF_RANGE=("$BASE_RESOLVED" "HEAD")
else
  echo "ERROR: base $BASE is unavailable (neither a commit nor a tree). The guard cannot run." >&2
  exit 1
fi

# An empty range is valid (for example, a commit without .rs files),
# but it must not mask the absence of a base, which was checked above.

# Only added lines in .rs files.
# `git diff` is invoked separately and its exit status is checked: `|| true`
# attached to the entire pipeline would hide a failure in git itself.
if ! diff_out=$(git diff "${DIFF_RANGE[@]}" -- '*.rs'); then
  echo "ERROR: git diff ${DIFF_RANGE[*]} did not run — guard cannot be checked." >&2
  exit 1
fi
# Use awk instead of `grep '^+' | grep -v '^+++'`: one command, always status 0,
# with nothing to mask. File headers (+++) are discarded.
added=$(printf '%s\n' "$diff_out" | awk '/^\+\+\+/ { next } /^\+/ { print }')

fail=0
check() {
  local pattern="$1" msg="$2"
  local hits
  # Use a here-string, not a pipeline: under pipefail, a pipeline ending in
  # `|| true` hides a source failure. grep without -q does not close the pipe early.
  hits=$(grep -E -- "$pattern" <<<"$added" || true)
  if [ -n "$hits" ]; then
    echo "FORBIDDEN: $msg" >&2
    echo "$hits" >&2
    echo "" >&2
    fail=1
  fi
}

check '#!?\[allow\(' 'new allow(...) — fix the cause instead of suppressing the lint'
check '#!?\[expect\(' 'new expect(...) — the same thing in different words'
check 'cfg_attr\(.*allow\(' 'lint suppression through cfg_attr'
check '#\[ignore\]' 'new #[ignore] — a disabled test does not count as a test'
check '\btodo!\(|\bunimplemented!\(' 'todo!/unimplemented! in code'
check '#\[cfg\(ignore\)\]' 'disabling code through cfg(ignore)'

# --- Changes to the guards themselves and their configuration ---
# Checks can be weakened outside the code as well: it is enough to remove -D warnings,
# exclude a module from mutation testing, or change this script itself.
# Paths are specified as directories: a directory pathspec covers everything beneath it
# and does not depend on globbing mode. Crate manifests are intentionally omitted here —
# the loss of `[lints] workspace = true` is caught by scripts/check-architecture.sh.
if ! policy_files=$(git diff --name-only "${DIFF_RANGE[@]}" -- \
  '.github/workflows' 'scripts' 'deny.toml' 'clippy.toml' \
  '.cargo/mutants.toml' 'Cargo.toml' 'tests/fixtures' \
  'flake.nix' 'flake.lock' 'rustfmt.toml'); then
  echo "ERROR: git diff --name-only did not run — guard cannot be checked." >&2
  exit 1
fi
if [ -n "$policy_files" ]; then
  echo "WARNING: quality-policy files changed:" >&2
  echo "$policy_files" >&2
  echo "Such changes are allowed only with justification in the bead description." >&2
  echo "Label the PR with 'policy-change'; otherwise, the guard will reject it." >&2
  if [ "${POLICY_CHANGE_APPROVED:-0}" != "1" ]; then
    fail=1
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "If the weakening is actually necessary, justify it in the bead description" >&2
  echo "and add an exclusion to this script in a separate commit." >&2
  exit 1
fi
echo "Diff-lint passed."