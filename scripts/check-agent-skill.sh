#!/usr/bin/env bash
# The agent document must not narrate the API (iaam-zu6m, epic E9).
#
# `docs/agent-skill/SKILL.md` told agents for weeks that three implemented route
# families answered `501`. Nothing had ever answered `501`; the routes worked.
# An agent that believed the document did not do work the system could do, and
# nothing failed, because a prose claim about a route has nothing checking it.
#
# The fix that survives is not a corrected sentence — it is a document that
# CANNOT make the claim. A running instance answers what it serves through
# `/.well-known/api-catalog` and the contract behind it; the document explains
# meaning. So this guard refuses a versioned route path, an HTTP method spelled
# as an instruction, and an HTTP status code, anywhere in the skill — the entry
# file and every companion file it names alike.
#
# Like the privacy guard, it checks SHAPES. It needs no list of routes, so it
# cannot itself go stale when the API changes — which is the whole point.
set -euo pipefail

if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "SKILL: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

DIR="docs/agent-skill"
DOC="$DIR/SKILL.md"
if [ ! -f "$DOC" ]; then
  echo "SKILL: $DOC is missing." >&2
  exit 1
fi

# `SKILL.md` is loaded whole and names companion files a caller opens when the
# work reaches them. A guard over the entry file alone would stop covering most
# of the skill the moment the first of those was written, so every markdown file
# in the directory is held to the same three refusals.
DOCS=()
while IFS= read -r f; do DOCS+=("$f"); done < <(find "$DIR" -type f -name '*.md' | sort)
if [ "${#DOCS[@]}" -eq 0 ]; then
  echo "SKILL: no markdown files under $DIR." >&2
  exit 1
fi

fail=0
err() { echo "SKILL: $*" >&2; fail=1; }

# A versioned route path. The single permitted address is the standards-defined
# entry point (RFC 9727): an agent needs one convention it already has, and that
# one is registered rather than invented here, so it cannot drift.
ROUTE_SHAPE='/v[0-9]+/'
# An HTTP method written as an instruction. Bare words are matched with word
# boundaries so ordinary prose containing them is not caught.
METHOD_SHAPE='\b(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b'
# Status codes, listed rather than matched as "three digits": the file
# legitimately contains RFC numbers and percentages.
STATUS_SHAPE='\b(200|201|202|204|301|302|304|400|401|403|404|405|409|410|415|422|429|500|501|502|503|504)\b'

check() {
  local shape=$1 what=$2 remedy=$3 doc hits
  for doc in "${DOCS[@]}"; do
    hits=$(grep -nE "$shape" "$doc" || true)
    if [ -n "$hits" ]; then
      err "$what in $doc:"
      printf '%s\n' "$hits" | head -10 >&2
      echo "        $remedy" >&2
    fi
  done
}

check "$ROUTE_SHAPE" "a route path" \
  "Say what the value MEANS. The address comes from the contract."
check "$METHOD_SHAPE" "an HTTP method" \
  "Name the operation by what it does, not by how it is called."
check "$STATUS_SHAPE" "an HTTP status code" \
  "The contract declares the responses; a copy here is a claim that rots."

# --- The guard tests its own boundary ----------------------------------------
# A guard nobody has seen fail is the advice it replaces.
probe_route=$(printf '%s\n' 'POST /v1/instruments отвечает 501' '/.well-known/api-catalog' \
  | grep -E "$ROUTE_SHAPE" || true)
if [ "$probe_route" != 'POST /v1/instruments отвечает 501' ]; then
  err "the route shape misclassifies its own probe"
  exit 1
fi
probe_status=$(printf '%s\n' 'маршрут отвечает 501' 'RFC 9727 и 13-15 %' \
  | grep -E "$STATUS_SHAPE" || true)
if [ "$probe_status" != 'маршрут отвечает 501' ]; then
  err "the status shape misclassifies its own probe"
  exit 1
fi
probe_method=$(printf '%s\n' 'читается через GET' 'the contract declares it' \
  | grep -E "$METHOD_SHAPE" || true)
if [ "$probe_method" != 'читается через GET' ]; then
  err "the method shape misclassifies its own probe"
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "SKILL: refused." >&2
  exit 1
fi
echo "Agent documents checked: ${#DOCS[@]}."
