#!/usr/bin/env bash
# Architecture guards (§3.1, §3.2 specification).
# Checks what the compiler does not check itself.
set -euo pipefail

# Guards run from the repository root regardless of where they are called.
# The root is found from the script directory, not the caller cwd: otherwise
# running from a non-Git directory produces an empty string, `cd ""`, and a guard
# that checks the wrong directory. Failure to find the root rejects the guard rather than passing it.
if ! REPO_ROOT=$(git -C "$(dirname -- "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null); then
  echo "ARCHITECTURE: could not determine the repository root from $(dirname -- "${BASH_SOURCE[0]}")" >&2
  exit 1
fi
cd "$REPO_ROOT"

fail=0
err() { echo "ARCHITECTURE: $*" >&2; fail=1; }

CORE_SRC="crates/iaam-core/src"

# Drops lines whose contents are comments.
# Without this, the guard fails on doc comments that explain the prohibition itself:
# the core header says “neither `async` nor `Mutex`” — that is correct code, not a violation.
# The input is the output of `grep -rn`, in the form “path:number:body”.
strip_comments() {
  awk '{
    body = $0
    sub(/^[^:]*:[0-9]+:/, "", body)
    if (body !~ /^[[:space:]]*(\/\/|\*\/|\*([[:space:]]|$)|\/\*)/) print
  }'
}

# The guard tests its own boundary: a dereference with an asterisk is
# executable Rust code, while a line comment containing the same arithmetic is not.
strip_probe=$(printf '%s\n' \
  'probe.rs:1: *x = y.checked_add(z)' \
  'probe.rs:2: // x.checked_add(z)' | strip_comments)
if [ "$strip_probe" != 'probe.rs:1: *x = y.checked_add(z)' ]; then
  err "strip_comments misclassifies a dereference or comment"
  printf '%s\n' "$strip_probe" >&2
fi

# cargo metadata is read ONCE: calling it four times during the guard creates
# four chances for one invocation to fail silently and let a violation pass.
# Failure of cargo metadata rejects the guard rather than making it pass.
meta_err=$(mktemp)
trap 'rm -f "$meta_err"' EXIT
if ! META=$(cargo metadata --no-deps --format-version 1 2>"$meta_err"); then
  echo "ARCHITECTURE: cargo metadata did not run — guard cannot be checked" >&2
  cat "$meta_err" >&2
  exit 1
fi
meta() { printf '%s' "$META"; }

# --- 1. iaam-core does not depend on any workspace crate ---
core_deps=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-core") | .dependencies[].name' \
  | { grep '^iaam-' || true; })
if [ -n "$core_deps" ]; then
  err "iaam-core depends on a workspace crate: $core_deps (§3.2)"
fi

# --- 2. The iaam-server library does not depend on adapters ---
# The composition root lives in the separate iaam-bootstrap crate: specific
# adapters must be assembled somewhere, but that is no reason for transport to know about SQLite.
bad=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-server") | .dependencies[]
           | select(.kind == null) | .name' \
  | { grep -E '^iaam-(store|market|ingest)$' || true; })
if [ -n "$bad" ]; then
  err "iaam-server depends on adapters: $bad — they belong in iaam-bootstrap (§3.2)"
fi

# --- 2a. An adapter knows only core ---
# iaam-store is a storage adapter. It converts domain types into database rows
# and back, so it must know core — but not the application, transport, or
# another adapter. A dependency in the opposite direction would turn the
# layers into a tangle and make “the shell does not calculate” unverifiable:
# the adapter would begin calculating.
bad=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-store") | .dependencies[]
           | select(.kind == null) | .name' \
  | { grep -E '^iaam-(app|server|bootstrap|ingest|market|broker)$' || true; })
if [ -n "$bad" ]; then
  err "iaam-store depends on higher-level layers: $bad (§3.2)"
fi

# --- 2b. The broker-access crate knows core and nobody else ---
# iaam-broker is an external-channel adapter: access encryption and clients
# for broker APIs. The BrokerChannel port lives in iaam-app because
# object-safe asynchronous traits exist only there; this crate does not need
# to know about the application, transport, or a neighbouring adapter.
# A separate note about iaam-store: the store holds ciphertext
# as opaque bytes, and a reverse dependency would mean that
# the storage adapter had taken responsibility for decrypting access.
bad=$(meta \
  | jq -r '.packages[] | select(.name=="iaam-broker") | .dependencies[]
           | select(.kind == null) | .name' \
  | { grep -E '^iaam-(app|server|bootstrap|store|ingest|market)$' || true; })
if [ -n "$bad" ]; then
  err "iaam-broker depends on higher-level layers or neighbouring adapters: $bad (§3.2)"
fi

# --- Data ingestion channels do not share parsing code (§10.3) ---
# Channel independence is not merely a declaration; it is a property of the code.
# If the API client starts calling the report parser, a shared error will distort
# both sides of reconciliation, and the accepted_independent level will become a lie
# that no test catches: reconciliation tests will see a match.
bad=$(grep -rn 'iaam_ingest::report' crates/iaam-broker/src 2>/dev/null || true)
if [ -n "$bad" ]; then
  err "iaam-broker uses the report parser: channels must be independent (§10.3)
$bad"
fi

# --- 3. No shared/common/utils crates ---
for forbidden in shared common utils; do
  if [ -d "crates/iaam-$forbidden" ]; then
    err "the iaam-$forbidden crate is forbidden (§3.2)"
  fi
done

# --- 4. The reference oracle must not enter production dependencies ---
# grep -q must not be used here: it closes the pipe, jq dies from SIGPIPE, and with
# pipefail the pipeline status becomes nonzero — meaning a real violation
# would be interpreted as “the check passed.” Capture the text, not the return code.
oracle_leak=$(meta \
  | jq -r '.packages[] | select(.name!="iaam-oracle") | .dependencies[]
           | select(.kind == null or .kind == "build") | .name' \
  | { grep -x 'iaam-oracle' || true; })
if [ -n "$oracle_leak" ]; then
  err "iaam-oracle appears in production or build dependencies (§15.4)"
fi

# --- 5. Binary floating point in core only in declared files ---
# Approximate mode (§6.6) lives in two files and only those files: the policy
# and a result with an error bound (approx.rs), and the rate solver itself
# (xirr.rs). The list is fixed by name, not by a directory pattern: a pattern
# would allow a third file with floating point to be added unnoticed.
APPROX_FILES=(
  "numeric/approx.rs"
  "numeric/xirr.rs"
)
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn '\bf64\b\|\bf32\b' "$CORE_SRC" --include='*.rs' || true)
  for allowed in "${APPROX_FILES[@]}"; do
    hits=$(printf '%s' "$hits" | { grep -v "^${CORE_SRC}/${allowed}:" || true; })
  done
  hits=$(printf '%s' "$hits" | strip_comments || true)
  if [ -n "$hits" ]; then
    err "binary floating point outside approximate mode (§6.6):"
    echo "$hits" >&2
  fi
fi

# --- 6. Core is synchronous and has no shared state ---
# Search for code constructs, not words: Mutex< and RwLock< with an angle bracket,
# and async fn with the keyword. Comments are discarded above.
if [ -d "$CORE_SRC" ]; then
  hits=$(grep -rn 'async fn\|\bMutex<\|\bRwLock<\|tokio::' "$CORE_SRC" --include='*.rs' \
    | strip_comments || true)
  if [ -n "$hits" ]; then
    err "async / Mutex / RwLock / tokio in core (§3.1):"
    echo "$hits" >&2
  fi
fi

# --- 7. Every crate inherits workspace lints ---
# unsafe is forbidden by the [workspace.lints.rust] table, but it applies
# to a crate only with [lints] workspace = true. A crate without this line
# silently escapes the prohibition, and nothing reports it.
for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  if ! awk '
      /^[[:space:]]*\[lints\]/            { in_lints = 1; next }
      /^[[:space:]]*\[/                   { in_lints = 0 }
      in_lints && /^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true/ { found = 1 }
      END                                 { exit !found }
    ' "$manifest"; then
    err "$manifest does not inherit workspace lints: the [lints] section with workspace = true is required (§15.1)"
  fi
done

# --- 8. Approximate mode does not grow into a shadow calculation layer ---
# Exempting a file from guard 5 is dangerous: monetary arithmetic could be
# placed in it. A size limit makes this visible. Each file has its own
# threshold: a solver with range scanning and error estimation is objectively
# longer than a policy declaration. ALL file lines are counted, including tests,
# just as they were counted for approx.rs; the threshold accounts for this.
APPROX_LIMITS=(
  "numeric/approx.rs:200"
  "numeric/xirr.rs:420"
)
for entry in "${APPROX_LIMITS[@]}"; do
  file="$CORE_SRC/${entry%%:*}"
  limit="${entry##*:}"
  [ -f "$file" ] || continue
  lines=$(wc -l < "$file")
  if [ "$lines" -gt "$limit" ]; then
    err "$file grew to $lines lines with a limit of $limit."
    err "Approximate mode must remain thin (§6.6)."
  fi
done

# --- 9. The shell does not calculate money ---
# The requirement from §3.1 and §13: every number in an API response comes from core.
# The guard searches for monetary arithmetic where it cannot belong: in application
# scenarios and transport. Ingestion (iaam-ingest) is intentionally excluded —
# it COLLECTS a fact from source fields rather than calculating a result,
# and prohibiting addition would make it impossible to implement.
SHELL_DIRS=("crates/iaam-app/src" "crates/iaam-server/src")
for dir in "${SHELL_DIRS[@]}"; do
  [ -d "$dir" ] || continue
  hits=$(grep -rnE '\.(try_add|try_sub|checked_add|checked_sub|checked_mul|checked_negate)\(' \
    "$dir" --include='*.rs' | strip_comments || true)
  if [ -n "$hits" ]; then
    err "monetary arithmetic in the shell ($dir): every number in the response must come from core (§3.1, §13)"
    echo "$hits" >&2
  fi
done

# --- 10. One asynchronous-trait mechanism ---
# §3.2 requires choosing one and enforcing it. async_trait was chosen, and it lives
# only in iaam-app: object-safe ports exist only there.
# Mixing mechanisms means having two ways to write the same thing and an endless
# dispute over which one should be used here.
for crate_dir in crates/*/src; do
  case "$crate_dir" in
    crates/iaam-app/src) continue ;;
  esac
  [ -d "$crate_dir" ] || continue
  hits=$(grep -rn 'async_trait' "$crate_dir" --include='*.rs' | strip_comments || true)
  if [ -n "$hits" ]; then
    err "async_trait outside iaam-app ($crate_dir): ports live only in the application (§3.2)"
    echo "$hits" >&2
  fi
done

# --- 11. Transport lives in one crate ---
# §3.1 and section 2.1 of the E3.2 design: source crates describe a request
# and parse a response, but know nothing about HTTP. MANIFESTS are checked,
# not source files: a declared but currently unused dependency is permission
# to use it tomorrow without a single change to the guard.
for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  case "$manifest" in
    crates/iaam-http/Cargo.toml) continue ;;
  esac
  hits=$(grep -n '^[[:space:]]*reqwest[[:space:]]*=' "$manifest" || true)
  if [ -n "$hits" ]; then
    err "$manifest declares reqwest: outgoing HTTP lives only in iaam-http (§3.1)"
    echo "$hits" >&2
  fi
done

# --- 12. Transport does not accept policy-derived states as price quality ---
# `PriceQualityDto` describes only values that an external source can assert.
# Carry-forward and staleness are computed inside the system and must not
# enter the fact journal through the public API.
hits=$(grep -nE '^[[:space:]]*(CarriedForward|Stale),' crates/iaam-server/src/dto.rs | strip_comments || true)
if [ -n "$hits" ]; then
  err "dto.rs exposes policy carry-forward as price quality (decision 0002)"
  echo "$hits" >&2
fi

if [ "$fail" -ne 0 ]; then
  echo "" >&2
  echo "Architecture guards did not pass. Fix the code, not the guard." >&2
  exit 1
fi
echo "Architecture guards passed."