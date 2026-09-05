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
# as an instruction, an HTTP status code, and the name of a payload field,
# anywhere in the skill.
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

# The skill is one file today (`iaam-jy0m`), and the walk over the directory is
# what keeps it one: a second file added beside it is held to the same four
# refusals from the moment it exists, rather than becoming the place the
# narration moves back into.
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
# A payload field name. A document that names `requiredScope` or `blocked_by` is
# narrating the payload, which is the disease the first three refusals treat one
# level down: the contract publishes those descriptions, they move with the code,
# and a copy here is a claim that rots. Matched as a shape — a backticked
# identifier in snake_case or lowerCamelCase — so it needs no list of fields and
# cannot go stale when the API changes.
FIELD_SHAPE='`[a-z][a-z0-9]*(_[a-z0-9]+|[A-Z][A-Za-z0-9]*)[A-Za-z0-9_]*`'

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
check "$FIELD_SHAPE" "a payload field name" \
  "Say what the value MEANS to him. The contract carries the field's own description."

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
probe_field=$(printf '%s\n' 'read `requiredScope` on the resolution' 'the money and the perimeter' \
  | grep -E "$FIELD_SHAPE" || true)
if [ "$probe_field" != 'read `requiredScope` on the resolution' ]; then
  err "the field shape misclassifies its own probe"
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "SKILL: refused." >&2
  exit 1
fi
echo "Agent documents checked: ${#DOCS[@]}."
