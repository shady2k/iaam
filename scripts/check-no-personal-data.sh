#!/usr/bin/env bash
# The operator's data must not be committed (CLAUDE.md).
#
# A rule in a document is advice. This is the guard, and it exists because the
# advice was written and broken in the same session: a person's capital, a
# month's income and expenses, and a merchant he pays reached a public
# repository and had to be removed by rewriting the history.
#
# The guard checks SHAPES, never values. It holds no list of anybody's accounts,
# merchants or amounts, and it needs no configuration to run.
#
# An earlier draft did keep such a list, built from the operator's own exports.
# It caught more — and it required the agent to hold his statements, and then an
# allow-list of which of his words were "not really his", assembled from the
# same files. The guard had become the thing it prevents. The list is gone; what
# replaced it is that the operator's data never reaches this side at all.
set -euo pipefail

if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "PRIVACY: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

fail=0
err() { echo "PRIVACY: $*" >&2; fail=1; }

# --- Layer 1: shapes, which need no list -------------------------------------

# Files that hold records rather than code. A synthetic fixture is welcome; a
# statement is not, and the two are told apart by where they live, because
# nothing in the bytes distinguishes them.
DATA_SUFFIXES='\.(csv|tsv|db|sqlite3?|xlsx?|xls|zip|ofx|qif|pdf)$'
# An import tool's fixtures belong to the tool: check-fixtures.sh freezes what
# lives under tests/fixtures/ and demands a Rust test name it, which a Python
# importer's sample cannot satisfy. The location is the whitelist, exactly as it
# is for the others — a real export dropped here would be a deliberate act, not
# an accident.
#
# The allowance names tools/, not .claude/skills/, and that is the whole point of
# where it points: the tools are vendor-neutral and have exactly one copy, while
# .claude/skills/ holds pointers (iaam-u7ns). A fixture copied back under
# .claude/ is refused here, before it can become a second source of truth that
# drifts from the first.
ALLOWED_DATA_DIRS='^(tests/fixtures/|crates/[^/]+/tests/fixtures/|tools/[^/]+/fixtures/|docs/)'
while IFS= read -r path; do
  [ -n "$path" ] || continue
  if ! printf '%s\n' "$path" | grep -qE "$ALLOWED_DATA_DIRS"; then
    err "data file outside the synthetic-fixture directories: $path"
    echo "        A real export must never be committed. If this file is invented," >&2
    echo "        put it under tests/fixtures/ where the fixture guard freezes it." >&2
  fi
done < <(git ls-files | grep -E "$DATA_SUFFIXES" || true)

# An amount as a statement prints it: groups of three separated by a space or a
# non-breaking space, with decimal comma. Checked only on lines this change
# ADDS, because the existing tree is vetted and old plans legitimately contain
# invented amounts in that shape.
AMOUNT_SHAPE='[0-9]{1,3}([ ][0-9]{3})+,[0-9]{2}'

# Which lines count as "added" depends on who is asking.
#
#   --staged   the pre-commit hook: what is about to BECOME a commit, so the
#              comparison is against the index. This is the only moment at which
#              a refusal costs a keystroke rather than a history rewrite.
#   <base>     the CI gate: what this branch added on top of the base commit.
#
# With neither, layer 2 cannot run at all. It used to skip in silence, and the
# pre-commit hook passed no base — so the hook reported "Personal data checked."
# while scanning nothing but tracked file names (iaam-rq4h). The scope is now
# printed, because a guard that will not say what it did not check is advice.
STAGED=0
BASE="${1:-${BASE:-}}"
if [ "$BASE" = "--staged" ]; then
  STAGED=1
  BASE=""
fi

added=""
if [ "$STAGED" -eq 1 ]; then
  scope="staged changes"
  added=$(git diff --cached -- '*.md' '*.rs' '*.sql' '*.toml' \
          | grep -E '^\+' | grep -v '^+++' | grep -E "$AMOUNT_SHAPE" || true)
elif [ -n "$BASE" ] && BASE_RESOLVED=$(git rev-parse --verify --quiet "${BASE}^{commit}"); then
  scope="tracked files, and lines added since $BASE"
  added=$(git diff "${BASE_RESOLVED}...HEAD" -- '*.md' '*.rs' '*.sql' '*.toml' \
          | grep -E '^\+' | grep -v '^+++' | grep -E "$AMOUNT_SHAPE" || true)
else
  scope="tracked file names only — no comparison base, so no added line was read"
fi
if [ -n "$added" ]; then
  err "an amount shaped like a statement's was added:"
  printf '%s\n' "$added" | head -5 >&2
  echo "        If it is invented, write it without the thousands separator." >&2
fi

# --- The guard tests its own boundary ----------------------------------------
# A guard nobody has seen fail is the advice it replaces.
probe_amount=$(printf '%s\n' '+ a total of 4 647 798,79 roubles' '+ a total of 1234.56' \
  | grep -E "$AMOUNT_SHAPE" || true)
if [ "$probe_amount" != '+ a total of 4 647 798,79 roubles' ]; then
  err "the amount shape misclassifies its own probe"
  exit 1
fi
probe_path=$(printf '%s\n' 'tests/fixtures/reports/synthetic.xlsx' \
  'tools/tbank-csv-import/fixtures/synthetic-export.csv' \
  '.claude/skills/tbank-csv-import/fixtures/synthetic-export.csv' \
  'statements/august.csv' \
  | grep -vE "$ALLOWED_DATA_DIRS" || true)
expected_refusals='.claude/skills/tbank-csv-import/fixtures/synthetic-export.csv
statements/august.csv'
if [ "$probe_path" != "$expected_refusals" ]; then
  err "the fixture-directory rule misclassifies its own probe"
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "PRIVACY: refused." >&2
  exit 1
fi
echo "Personal data checked ($scope)."
