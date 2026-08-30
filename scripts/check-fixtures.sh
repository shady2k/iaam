#!/usr/bin/env bash
# Frozen reference fixtures (§15.7).
# The agent must not edit expected values just to fix a failing test.
set -euo pipefail

# Determine the root from the script's directory, not from cwd (see check-architecture.sh).
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "FIXTURES: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

FIXTURE_DIR="tests/fixtures"
MANIFEST="$FIXTURE_DIR/MANIFEST.sha256"

if [ ! -f "$MANIFEST" ]; then
  echo "Manifest $MANIFEST is missing." >&2
  exit 1
fi

# 1. Fixture contents have not changed
if ! sha256sum -c "$MANIFEST" --quiet; then
  echo "" >&2
  echo "A frozen fixture changed (§15.7)." >&2
  echo "Expected values come from an independent source and must not be edited" >&2
  echo "just to make the test pass. If the change is justified, update the manifest" >&2
  echo "in a SEPARATE commit with justification and owner approval:" >&2
  echo "  sha256sum $FIXTURE_DIR/*.json > $MANIFEST" >&2
  exit 1
fi

# Paths from the manifest. sha256sum format: <64 hex><space><' ' or '*'><path>.
# A line that does not match this pattern is discarded here and is also
# ignored by sha256sum -c itself (it only prints a warning and returns 0),
# so its absence from the paths below causes rejection at step 3.
manifest_paths=$(sed -nE 's/^[0-9a-fA-F]{64} [ *](.+)$/\1/p' "$MANIFEST")

if [ -z "$manifest_paths" ]; then
  echo "Manifest $MANIFEST contains no valid checksum lines." >&2
  exit 1
fi

# 2. Every fixture from the manifest is actually read by tests
missing=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  name=$(basename -- "$path")
  # grep -q is safe here: this is a simple command, not the end of a pipeline,
  # so there is nothing for it to close early. -F searches for the filename
  # as literal text, so dots and other regex metacharacters do not broaden the search.
  # --include must appear BEFORE --: after -- it becomes a filename operand,
  # the *.rs filter is silently not applied (a mention in a crate README
  # would count as a test reference), and grep returns 2 because the “file”
  # --include=*.rs cannot be found; the result then depends on directory traversal order.
  if ! grep -rqF --include='*.rs' -- "$name" crates/; then
    echo "Fixture $name is not mentioned by any test — dead reference." >&2
    missing=1
  fi
done <<<"$manifest_paths"

# 3. tests/fixtures/ contains no files outside the manifest
# Without this check, an unfrozen reference—a file added to the directory but
# not added to the manifest—passes the guard: sha256sum -c checks only what
# is listed and says nothing about anything else.
unmanifested=$(comm -13 \
  <(printf '%s\n' "$manifest_paths" | LC_ALL=C sort) \
  <(find "$FIXTURE_DIR" -type f ! -name 'MANIFEST.sha256' -print | LC_ALL=C sort))
if [ -n "$unmanifested" ]; then
  echo "Files in $FIXTURE_DIR outside the manifest — unfrozen references (§15.7):" >&2
  echo "$unmanifested" >&2
  echo "Add them to $MANIFEST or delete them." >&2
  missing=1
fi

if [ "$missing" -ne 0 ]; then
  exit 1
fi

echo "Fixtures checked."