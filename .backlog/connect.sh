#!/usr/bin/env bash
# Connect this clone to the backlog workflow: `make backlog-connect`.
#
# Scope is personal (.backlog/config.json): the hook blocks are committed, but
# they act only in a clone that ran this, which records `iaam.backlog=on` in its
# own git config. A clone that never connected commits as before.
#
# Safe to rerun. It checks every file and tool a hook reads, and fails with what
# is missing rather than connecting half.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$REPO_ROOT"

missing=0
need() { if ! "$@" >/dev/null 2>&1; then echo "CONNECT: missing: $*" >&2; missing=1; fi; }
need command -v node
need command -v bd
for f in .backlog/config.json .backlog/adapter.mjs .backlog/gate.mjs .backlog/commits.mjs \
         .backlog/rules/check.mjs .backlog/rules/check-commits.mjs .backlog/rules/check-docs.mjs; do
  need test -r "$f"
done
if command -v node >/dev/null 2>&1; then
  major=$(node -p 'process.versions.node.split(".")[0]')
  [ "$major" -ge 18 ] || { echo "CONNECT: node $major is too old; the checks need 18 or newer" >&2; missing=1; }
fi
if [ "$missing" -ne 0 ]; then
  echo "CONNECT: refused. Nothing was changed; install what is missing and rerun make backlog-connect." >&2
  exit 1
fi

# The tracker first: a fresh clone has the committed export but no database,
# and every hook below reads the database. Bootstrap never deletes issues.
bd bootstrap --yes >/dev/null || { echo "CONNECT: bd bootstrap failed; the tracker is not readable in this clone" >&2; exit 1; }
# Beads points git at .beads/hooks, the committed hooks this installation extends.
[ "$(git config --get core.hooksPath)" = ".beads/hooks" ] || bd hooks install --beads >/dev/null \
  || { echo "CONNECT: bd hooks install --beads failed" >&2; exit 1; }

# The project's own hooks (privacy guard, worktree sweep), then ours into the
# same directory git actually consults.
./scripts/install-hooks.sh

hooks_dir=$(git config --get core.hooksPath || echo "$(git rev-parse --git-dir)/hooks")
case "$hooks_dir" in /*) ;; *) hooks_dir="$REPO_ROOT/$hooks_dir" ;; esac

IFS= read -r -d '' PRE_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM BACKLOG GATE (iaam-oik0) ---
# Managed by .backlog/connect.sh, outside the beads markers so it survives
# `bd hooks install`. Acts only in a clone that ran make backlog-connect.
if [ "$(git config --get iaam.backlog)" = "on" ]; then
  _iaam_root=$(git rev-parse --show-toplevel) || exit 1
  if [ ! -r "$_iaam_root/.backlog/gate.mjs" ] || ! command -v node >/dev/null 2>&1; then
    echo "BACKLOG: this clone is connected but .backlog/gate.mjs or node is missing." >&2
    echo "         Refusing the commit: a gate that cannot run must not pass. Run: make backlog-connect" >&2
    exit 1
  fi
  node "$_iaam_root/.backlog/gate.mjs" >&2 || exit 1
fi
# --- END IAAM BACKLOG GATE (iaam-oik0) ---
BLOCK

IFS= read -r -d '' MSG_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM COMMIT LINKS (iaam-oik0) ---
# Managed by .backlog/connect.sh. Acts only in a clone that ran make backlog-connect.
if [ "$(git config --get iaam.backlog)" = "on" ]; then
  _iaam_root=$(git rev-parse --show-toplevel) || exit 1
  if [ ! -r "$_iaam_root/.backlog/commits.mjs" ] || ! command -v node >/dev/null 2>&1; then
    echo "COMMIT LINKS: this clone is connected but .backlog/commits.mjs or node is missing." >&2
    echo "              Refusing the commit. Run: make backlog-connect" >&2
    exit 1
  fi
  node "$_iaam_root/.backlog/commits.mjs" --message-file "$1" || exit 1
fi
# --- END IAAM COMMIT LINKS (iaam-oik0) ---
BLOCK

ensure_block() {
  local hook="$1" mark="$2" block="$3"
  [ -e "$hook" ] || printf '%s\n' '#!/usr/bin/env sh' > "$hook"
  if ! grep -Fq "$mark" "$hook"; then
    if grep -qE '^[[:space:]]*exec[[:space:]]' "$hook"; then
      echo "CONNECT: $hook ends in an exec; a block appended after it would never run." >&2
      exit 1
    fi
    printf '%s\n' "$block" >> "$hook"
  fi
  chmod +x "$hook"
  sh -n "$hook" || { echo "CONNECT: $hook is not a valid shell script" >&2; exit 1; }
}
ensure_block "$hooks_dir/pre-commit" '# --- BEGIN IAAM BACKLOG GATE' "$PRE_BLOCK"
ensure_block "$hooks_dir/commit-msg" '# --- BEGIN IAAM COMMIT LINKS' "$MSG_BLOCK"

git config iaam.backlog on

# Prove it runs here, now. A red verdict is the gate working; only an inability
# to run (exit 2) is a failed connection.
rc=0
node .backlog/gate.mjs >/dev/null 2>&1 || rc=$?
if [ "$rc" -eq 2 ]; then
  git config --unset iaam.backlog
  echo "CONNECT: the backlog gate cannot run in this clone (exit 2); disconnected again:" >&2
  node .backlog/gate.mjs >&2 || true
  exit 1
fi
[ "$rc" -eq 0 ] || echo "note: the backlog gate currently reports NEW problems; run: node .backlog/gate.mjs"
echo "connected: $hooks_dir/pre-commit runs the backlog gate, $hooks_dir/commit-msg the commit-link check."
