#!/usr/bin/env bash
# Install the repository's git hooks.
#
# The privacy guard runs in `make check`, which catches a value before it is
# pushed. The hook catches it before it enters a commit at all — and the
# difference is a history rewrite, which this project has already paid for once.
set -euo pipefail
REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
HOOK="$REPO_ROOT/.git/hooks/pre-commit"
cat > "$HOOK" <<'HOOK_BODY'
#!/usr/bin/env bash
set -euo pipefail
ROOT=$(git rev-parse --show-toplevel)
exec "$ROOT/scripts/check-no-personal-data.sh"
HOOK_BODY
chmod +x "$HOOK"
echo "installed: $HOOK"
echo "Set IAAM_PRIVATE_SOURCES in your shell profile, or the hook will refuse:"
echo "  export IAAM_PRIVATE_SOURCES=/path/to/export.csv:/path/to/budget.zip"
echo "  export IAAM_PRIVATE_SOURCES=none   # if this checkout has no such files"
