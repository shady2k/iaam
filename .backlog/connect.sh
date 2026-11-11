#!/usr/bin/env bash
# Connect this clone to the backlog workflow: `make backlog-connect`.
#
# Scope is personal (.backlog/config.json): the hooks act only in a clone that
# ran this, which records `iaam.backlog=on` in its own git config, and CI
# enforces nothing of it.
#
# THE TRACKER LAYOUT (iaam-ima7z): the tracker's store lives off the branches,
# one per machine, on the ref refs/tracker/state, and every checkout and
# worktree of the machine reaches it through its `.beads` link. This script
#   1. checks node, git, br, systemctl and every file a hook reads;
#   2. points git back at .git/hooks (the hooks are composed there, never in a
#      folder of the tree, because the store's record step carries this
#      machine's own paths);
#   3. runs the mechanism's connect: fetches the store's ref whole, builds this
#      machine's database from it, links every checkout and worktree to it and
#      composes the record hooks (post-commit, post-merge, post-checkout);
#   4. installs and starts the background sync as the user service
#      iaam-tracker-sync, and proves it is running;
#   5. composes the project's hooks: privacy guard and worktree sweep
#      (scripts/install-hooks.sh), backlog gate, tracker layout and product
#      documents (pre-commit), commit links (commit-msg) and the push range
#      (pre-push);
#   6. runs the gate once.
#
# Safe to rerun. It fails with what is missing rather than connecting half.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
MAIN_ROOT=$(cd "$(git -C "$REPO_ROOT" rev-parse --git-common-dir)/.." && pwd)
cd "$REPO_ROOT"
MECH="$MAIN_ROOT/.backlog/rules/tracker-sync.mjs"
RECIPE="$MAIN_ROOT/.backlog/rules/trackers/br.mjs"
UNIT=${IAAM_SYNC_UNIT:-iaam-tracker-sync}.service  # the variable is for a proof in a scratch clone

missing=0
need() { if ! "$@" >/dev/null 2>&1; then echo "CONNECT: missing: $*" >&2; missing=1; fi; }
need command -v node
need command -v git
need command -v br
need command -v systemctl
for f in config.json adapter.mjs gate.mjs commits.mjs ready.mjs supervisors/tracker-sync.service \
         rules/check.mjs rules/time-format.mjs rules/check-commits.mjs rules/check-docs.mjs \
         rules/check-present.mjs rules/document-format.mjs rules/check-product.mjs \
         rules/regular-file.mjs rules/argv.mjs rules/tracker-sync.mjs rules/tracker-sync.version \
         rules/trackers/br.mjs; do
  need test -r ".backlog/$f"
  [ "$REPO_ROOT" = "$MAIN_ROOT" ] || need test -r "$MAIN_ROOT/.backlog/$f"
done
if command -v node >/dev/null 2>&1; then
  major=$(node -p 'process.versions.node.split(".")[0]')
  [ "$major" -ge 18 ] || { echo "CONNECT: node $major is too old; the checks need 18 or newer" >&2; missing=1; }
fi
if [ "$missing" -ne 0 ]; then
  echo "CONNECT: refused. Nothing was changed; install what is missing and rerun make backlog-connect." >&2
  exit 1
fi

# 2. Hooks in .git/hooks. Before the move they lived in .beads/hooks, which is
# now the store's link; an in-tree core.hooksPath would make the mechanism's
# connect refuse, and rightly.
case "$(git config --get core.hooksPath || true)" in
  "") ;;
  .beads/hooks|"$MAIN_ROOT/.beads/hooks") git config --unset core.hooksPath ;;
  *) echo "CONNECT: core.hooksPath is $(git config --get core.hooksPath); this project composes its hooks in .git/hooks. Unset it and rerun." >&2; exit 1 ;;
esac
hooks_dir="$(git rev-parse --git-common-dir)/hooks"
case "$hooks_dir" in /*) ;; *) hooks_dir="$REPO_ROOT/$hooks_dir" ;; esac
mkdir -p "$hooks_dir"

# 3. The store. Every checkout and worktree of this machine is pointed at it.
# Named one by one rather than with --all: the store is itself a worktree of this
# repository, and the mechanism's --all (0.143.0) tries to point the store at itself.
checkouts=()
while IFS= read -r wt; do
  [ -e "$wt/.tracker-sync" ] && continue
  checkouts+=(--checkout "$wt")
done < <(git worktree list --porcelain | sed -n 's/^worktree //p')
node "$MECH" connect "${checkouts[@]}" --prefix iaam --recipe "$RECIPE" --repo "$MAIN_ROOT" \
  || { echo "CONNECT: the tracker store could not be connected (above); nothing else was changed." >&2; exit 1; }

# 4. The background sync: publishes a change within seconds, takes the other
# machines' snapshots every 10 s, and survives logout and reboot (linger).
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$unit_dir"
# The machine's state directory is the one the store was just made in, named in
# the unit so the service never derives it from an environment of its own.
STORE=$(node "$MECH" status --json --recipe "$RECIPE" --repo "$MAIN_ROOT" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).store||""))')
[ -n "$STORE" ] || { echo "CONNECT: the mechanism's status names no store" >&2; exit 1; }
STATE=$(dirname "$STORE")
sed -e "s|<project>|iaam|" \
    -e "s|<the project's integration, the Tracker synchronization field>|file://$MAIN_ROOT/docs/backlog-integration.md|" \
    -e "s|<repo>|$MAIN_ROOT|g" -e "s|<mechanism>|$MECH|" -e "s|<recipe>|$RECIPE|" -e "s|<state>|$STATE|" \
    .backlog/supervisors/tracker-sync.service > "$unit_dir/$UNIT.new"
if ! cmp -s "$unit_dir/$UNIT.new" "$unit_dir/$UNIT" 2>/dev/null; then
  mv "$unit_dir/$UNIT.new" "$unit_dir/$UNIT"
  systemctl --user daemon-reload
  systemctl --user enable "$UNIT" >/dev/null
  systemctl --user restart "$UNIT"
else
  rm -f "$unit_dir/$UNIT.new"
  systemctl --user enable "$UNIT" >/dev/null
  systemctl --user is-active --quiet "$UNIT" || systemctl --user start "$UNIT"
fi
sleep 2
if ! systemctl --user is-active --quiet "$UNIT"; then
  echo "CONNECT: the background sync $UNIT is not running:" >&2
  systemctl --user status "$UNIT" --no-pager -n 20 >&2 || true
  exit 1
fi
[ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || echo no)" = "yes" ] \
  || echo "note: linger is off for $USER, so the sync stops at logout. Turn it on with: loginctl enable-linger $USER"

# 5. The project's hooks, composed into the directory git consults.
./scripts/install-hooks.sh

IFS= read -r -d '' PRE_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM BACKLOG GATE (iaam-oik0) ---
# Managed by .backlog/connect.sh. Acts only in a clone that ran make backlog-connect.
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

IFS= read -r -d '' LAYOUT_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM TRACKER OFF BRANCHES (iaam-ima7z) ---
# Managed by .backlog/connect.sh. The tracker lives on refs/tracker/state, off
# every branch: no branch commits anything under .beads.
if [ "$(git config --get iaam.backlog)" = "on" ]; then
  _iaam_staged=$(git diff --cached --name-only -- .beads)
  if [ -n "$_iaam_staged" ]; then
    echo "TRACKER: tracker files are staged:" >&2
    echo "$_iaam_staged" | sed 's/^/         /' >&2
    echo "         The tracker lives off the branches (refs/tracker/state) and its sync publishes it." >&2
    echo "         Unstage them and commit the rest: git restore --staged .beads" >&2
    exit 1
  fi
fi
# --- END IAAM TRACKER OFF BRANCHES (iaam-ima7z) ---
BLOCK

IFS= read -r -d '' PRODUCT_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM PRODUCT DOCUMENTS (iaam-ima7z) ---
# Managed by .backlog/connect.sh. Product documents under docs/ keep their form.
if [ "$(git config --get iaam.backlog)" = "on" ]; then
  _iaam_root=$(git rev-parse --show-toplevel) || exit 1
  if [ ! -r "$_iaam_root/.backlog/rules/check-product.mjs" ]; then
    echo "PRODUCT DOCUMENTS: .backlog/rules/check-product.mjs is missing. Run: make backlog-connect" >&2
    exit 1
  fi
  node "$_iaam_root/.backlog/rules/check-product.mjs" --root "$_iaam_root" --staged >&2 || exit 1
fi
# --- END IAAM PRODUCT DOCUMENTS (iaam-ima7z) ---
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

IFS= read -r -d '' PUSH_BLOCK <<'BLOCK' || true

# --- BEGIN IAAM PUSH LINKS (iaam-ima7z) ---
# Managed by .backlog/connect.sh. Every commit a push introduces names its task.
if [ "$(git config --get iaam.backlog)" = "on" ]; then
  _iaam_root=$(git rev-parse --show-toplevel) || exit 1
  if [ ! -r "$_iaam_root/.backlog/commits.mjs" ] || ! command -v node >/dev/null 2>&1; then
    echo "PUSH LINKS: this clone is connected but .backlog/commits.mjs or node is missing." >&2
    echo "            Refusing the push. Run: make backlog-connect" >&2
    exit 1
  fi
  node "$_iaam_root/.backlog/commits.mjs" --push "$1" || exit 1
fi
# --- END IAAM PUSH LINKS (iaam-ima7z) ---
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
# The block the layout before the move installed refused the export off main only.
if grep -Fq '# --- BEGIN IAAM TRACKER LAYOUT (iaam-zgcu) ---' "$hooks_dir/pre-commit" 2>/dev/null; then
  sed -i '/# --- BEGIN IAAM TRACKER LAYOUT (iaam-zgcu) ---/,/# --- END IAAM TRACKER LAYOUT (iaam-zgcu) ---/d' "$hooks_dir/pre-commit"
fi
ensure_block "$hooks_dir/pre-commit" '# --- BEGIN IAAM BACKLOG GATE' "$PRE_BLOCK"
ensure_block "$hooks_dir/pre-commit" '# --- BEGIN IAAM TRACKER OFF BRANCHES' "$LAYOUT_BLOCK"
ensure_block "$hooks_dir/pre-commit" '# --- BEGIN IAAM PRODUCT DOCUMENTS' "$PRODUCT_BLOCK"
ensure_block "$hooks_dir/commit-msg" '# --- BEGIN IAAM COMMIT LINKS' "$MSG_BLOCK"
ensure_block "$hooks_dir/pre-push" '# --- BEGIN IAAM PUSH LINKS' "$PUSH_BLOCK"

git config iaam.backlog on

# The gate's baseline is the store's recording of HEAD. A revision cut before
# this clone was connected has none here; recording it now is recording the
# revision this checkout stands on, and one another machine recorded is replayed.
node "$MECH" record --revision "$(git rev-parse HEAD)" --recipe "$RECIPE" --repo "$MAIN_ROOT" >/dev/null \
  || echo "note: the store could not record HEAD (above); the gate needs --base <a recorded revision> until the next commit"

# 6. Prove it runs here, now. A red verdict is the gate working; only an
# inability to run (exit 2) is a failed connection.
rc=0
node .backlog/gate.mjs --no-recover >/dev/null 2>&1 || rc=$?
if [ "$rc" -eq 2 ]; then
  git config --unset iaam.backlog
  echo "CONNECT: the backlog gate cannot run in this clone (exit 2); disconnected again:" >&2
  node .backlog/gate.mjs --no-recover >&2 || true
  exit 1
fi
[ "$rc" -eq 0 ] || echo "note: the backlog gate currently reports NEW problems; run: node .backlog/gate.mjs"
echo "connected: the tracker store on refs/tracker/state, synced by $UNIT; hooks in $hooks_dir:"
echo "  pre-commit (privacy, backlog gate, tracker off branches, product documents), commit-msg (links), pre-push (links of what a push introduces),"
echo "  post-commit/post-merge/post-checkout (the store records each revision's state)."
