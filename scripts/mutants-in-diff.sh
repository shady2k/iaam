#!/usr/bin/env bash
# Mutants ONLY in changed lines — a fast development-loop check.
#
# This is NOT the guard. The guard is scripts/check-mutants.sh; it runs the full
# list of critical modules and is the script invoked in CI. This script checks
# mutants only on lines touched by the diff:
# changing a test in one module can revive a mutant in another,
# and a diff-only check will not catch that.
#
# The point is the cost of feedback. A full run means thousands of mutants and hours;
# after changing one file, only dozens of them are relevant.
set -euo pipefail

if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "DIFF-MUTANTS: could not determine the repository root" >&2
  exit 1
fi
cd "$REPO_ROOT"

BASE="${BASE:-main}"

for tool in cargo git; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "DIFF-MUTANTS: $tool is unavailable." >&2
    exit 1
  fi
done
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "DIFF-MUTANTS: cargo-mutants is unavailable." >&2
  exit 1
fi

# Three-dot comparison uses the divergence point, not the current state of the
# base branch. Otherwise, other people's commits in main appear in the diff as ours.
if ! git rev-parse --verify --quiet "$BASE" >/dev/null; then
  echo "DIFF-MUTANTS: branch '$BASE' was not found (set BASE=...)." >&2
  exit 1
fi

DIFF_FILE=$(mktemp)
trap 'rm -f "$DIFF_FILE"' EXIT

# Uncommitted changes are intentionally included in the diff: we want to check
# what is written now, not just what has already been committed.
#
# Use one diff from the divergence point to the working tree, rather than
# concatenating `BASE...HEAD` and `HEAD`. Concatenation produced two sets of
# headers for the same path when a file was changed both in branch commits and
# in the working tree; cargo-mutants cannot apply such a diff and exits with a
# status indistinguishable from surviving mutants (iaam-387k).
if ! MERGE_BASE=$(git merge-base "$BASE" HEAD 2>/dev/null); then
  echo "DIFF-MUTANTS: HEAD and '$BASE' have no common ancestor — cannot build the diff." >&2
  exit 1
fi
if ! git diff "$MERGE_BASE" >"$DIFF_FILE" 2>/dev/null; then
  echo "DIFF-MUTANTS: could not build the diff from $MERGE_BASE." >&2
  echo "This is a diff construction failure, not surviving mutants." >&2
  exit 1
fi

if [ ! -s "$DIFF_FILE" ]; then
  echo "DIFF-MUTANTS: nothing has changed relative to $BASE — nothing to check."
  exit 0
fi

echo "Mutants in lines changed relative to $BASE."
echo "WARNING: this is not the guard. Full check: make mutants."
echo ""

# Use --error only when the diff touches iaam-core: the types are declared there,
# and in any other package such a mutant will never be built, while producing
# this output incurs the cost of a full build (see scripts/check-mutants.sh).
error_args=()
if grep -q '^+++ b/crates/iaam-core/' "$DIFF_FILE"; then
  error_args=(
    --error 'crate::numeric::NumericError::Overflow'
    --error 'crate::money::MoneyError::Overflow'
  )
fi

cargo mutants \
  --in-diff "$DIFF_FILE" \
  "${error_args[@]}" \
  --profile mutant \
  --jobs 1 \
  --output target/mutants-in-diff \
  "$@"