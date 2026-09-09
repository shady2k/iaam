#!/usr/bin/env bash
# Reclaim build space.
#
# An agent working in this repository opens a git worktree per task, and every
# worktree carries its own `target/`. Three of those building at once is tens of
# gigabytes, and a worktree left registered after its branch is merged will fill
# up again the next time anything builds in it.
#
# This script removes what is provably finished and touches nothing else:
# a worktree whose branch is an ancestor of HEAD has nothing left to lose.
# A worktree that is locked, or whose branch is not merged, is reported and
# kept — a half-finished piece of work is not build residue.
set -euo pipefail

cd "$(dirname "$0")/.."

before=$(df -h . | awk 'NR==2 {print $4}')

removed=0
kept=0

while read -r path _ branch _; do
    [ "$path" = "$(pwd)" ] && continue
    case "$path" in
    */.claude/worktrees/*) ;;
    *) continue ;;
    esac

    name=${branch#[}
    name=${name%]}

    if [ -f "$(git rev-parse --git-dir)/worktrees/$(basename "$path")/locked" ]; then
        echo "kept (in use):     $path"
        kept=$((kept + 1))
        continue
    fi
    if ! git merge-base --is-ancestor "$name" HEAD 2>/dev/null; then
        echo "kept (unmerged):   $path  [$name]"
        kept=$((kept + 1))
        continue
    fi

    git worktree remove --force "$path"
    git branch -D "$name" >/dev/null 2>&1 || true
    echo "removed:           $path"
    removed=$((removed + 1))
done < <(git worktree list)

git worktree prune

echo
echo "Worktrees removed: $removed, kept: $kept"
echo "Free space: $before -> $(df -h . | awk 'NR==2 {print $4}')"
echo
echo "The main target/ is not touched: it is the working build. To reclaim it,"
echo "'cargo clean' costs a full rebuild, and 'rm -rf target/debug/incremental'"
echo "costs only the incremental cache."
