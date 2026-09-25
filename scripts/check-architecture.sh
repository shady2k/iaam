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
# and parse a response, but know nothing about HTTP. Dependencies are checked,
# not source files: a declared but currently unused dependency is permission
# to use it tomorrow without a single change to the guard.
#
# The check reads what cargo resolved, not the manifest text, because a
# manifest can name a crate three ways and only one of them spells it:
# `reqwest = …`, `wire = { package = "reqwest" }` and `reqwest.workspace = true`
# all arrive in cargo metadata as a dependency whose `name` is `reqwest`. Any
# other client is refused as well: a second HTTP stack is the same bypass
# under another name. A `-suffix` crate of one of them (`hyper-util`,
# `reqwest-middleware`) is the same stack.
#
# The limit: the list names the clients known when it was written. A client
# crate missing from it passes this guard and is left to review; nothing in
# cargo metadata says that a crate speaks HTTP.
HTTP_CLIENT_CRATES='^(reqwest|hyper|ureq|isahc|surf|curl|attohttpc|awc|minreq|http_req|ehttp)(-.*)?$'
http_client_deps() {
  jq -r --arg crates "$HTTP_CLIENT_CRATES" '
    .packages[] | select(.name != "iaam-http") | .name as $package
    | .dependencies[] | select(.name | test($crates))
    | "\($package) depends on \(.name)"
      + (if .rename then " renamed \(.rename)" else "" end)
      + " (\(.kind // "normal"))"'
}
# The workspace root can declare a dependency that no member uses yet, and a
# member inherits it with one line. cargo metadata shows only what members
# use, so the root's own tables are read as text: a key or a `package = …`
# naming a client, in `[workspace.dependencies]` or a `[workspace.dependencies.x]`
# table, is refused. Only iaam-http may use such a crate, and it declares it itself.
workspace_http_client_deps() {
  awk -v crates="$HTTP_CLIENT_CRATES" '
    function strip(s) { gsub(/["'\''[:space:]]/, "", s); return s }
    /^[[:space:]]*\[/ {
      header = $0
      sub(/#.*/, "", header); header = strip(header)
      in_deps = header ~ /^\[workspace\.dependencies(\.|\])/
      if (header ~ /^\[workspace\.dependencies\./) {
        key = header; sub(/^\[workspace\.dependencies\./, "", key); sub(/\]$/, "", key)
        if (key ~ crates) print FILENAME ":" FNR ": " $0
      }
      next
    }
    !in_deps { next }
    {
      body = $0; sub(/#.*/, "", body)
      key = body; sub(/[.=].*/, "", key); key = strip(key)
      named = ""
      if (match(body, /package[[:space:]]*=[[:space:]]*"[^"]*"/)) {
        named = substr(body, RSTART, RLENGTH); sub(/^[^"]*"/, "", named); sub(/"$/, "", named)
      }
      if ((key != "" && key ~ crates) || (named != "" && named ~ crates)) print FILENAME ":" FNR ": " $0
    }
  ' "$@"
}

# The guard tests its own boundary on every run: an invented workspace in
# which iaam-http holds reqwest (allowed) and other members hold it under its
# own name, under another name, via the workspace, as another client, and a
# member holds only an unrelated crate.
http_probe_meta='{"packages":[
  {"name":"iaam-http","dependencies":[{"name":"reqwest","rename":null,"kind":null}]},
  {"name":"plain","dependencies":[{"name":"reqwest","rename":null,"kind":null}]},
  {"name":"renamed","dependencies":[{"name":"reqwest","rename":"wire","kind":null}]},
  {"name":"inherited","dependencies":[{"name":"reqwest","rename":null,"kind":"dev"}]},
  {"name":"other","dependencies":[{"name":"ureq","rename":null,"kind":null},{"name":"hyper-util","rename":null,"kind":"build"}]},
  {"name":"small","dependencies":[{"name":"minreq","rename":null,"kind":null},{"name":"http_req","rename":null,"kind":null},{"name":"ehttp","rename":null,"kind":"dev"}]},
  {"name":"clean","dependencies":[{"name":"serde","rename":null,"kind":null},{"name":"curlew","rename":null,"kind":null}]}
]}'
http_probe=$(printf '%s' "$http_probe_meta" | http_client_deps | tr '\n' '|')
expected='plain depends on reqwest (normal)|renamed depends on reqwest renamed wire (normal)|inherited depends on reqwest (dev)|other depends on ureq (normal)|other depends on hyper-util (build)|small depends on minreq (normal)|small depends on http_req (normal)|small depends on ehttp (dev)|'
if [ "$http_probe" != "$expected" ]; then
  err "the HTTP-client dependency guard misclassifies its probe: got '$http_probe'"
fi

probe_dir=$(mktemp -d)
trap 'rm -f "$meta_err"; rm -rf "$probe_dir"' EXIT
cat > "$probe_dir/Cargo.toml" <<'PROBE'
[workspace]
members = ["crates/a"]

[workspace.dependencies]
serde = "1"
"reqwest" = "0.13"
wire = { version = "0.13", package = "reqwest" }
curlew = "1"

[workspace.dependencies.ureq]
version = "2"

[workspace.dependencies.transport]
package = "isahc"

[workspace.lints.rust]
reqwest = "allow"
PROBE
workspace_probe=$(workspace_http_client_deps "$probe_dir/Cargo.toml" | sed 's|^.*/||' | cut -d: -f1,2 | tr '\n' ' ')
if [ "$workspace_probe" != 'Cargo.toml:6 Cargo.toml:7 Cargo.toml:10 Cargo.toml:14 ' ]; then
  err "the workspace HTTP-client guard misclassifies its probe: got '$workspace_probe'"
fi

hits=$(meta | http_client_deps)
if [ -n "$hits" ]; then
  err "an HTTP client crate outside iaam-http: outgoing HTTP lives only in iaam-http (§3.1)"
  echo "$hits" >&2
fi
hits=$(workspace_http_client_deps Cargo.toml)
if [ -n "$hits" ]; then
  err "the workspace root declares an HTTP client crate: iaam-http declares its own, and no member may inherit one (§3.1)"
  echo "$hits" >&2
fi

# Rust source with comments and literals removed, one output line per source
# line, as `path:line:code`. The guards below look for code, and a word in a
# doc comment explaining the prohibition, or in a string an error prints, is
# not code: without this, the guard fires on its own documentation.
# A string is kept as `""` and a character as `' '`, so the code around them
# still reads as code; a raw string (`r#"…"#`) ends only at its own closing
# quote and hashes, and a block comment nests as Rust nests it.
rust_code() {
  [ "$#" -gt 0 ] || return 0
  LC_ALL=C.UTF-8 awk '
    FNR == 1 { block = 0; str = 0; raw = -1 }
    {
      line = $0; n = length(line); out = ""; i = 1
      while (i <= n) {
        c = substr(line, i, 1); two = substr(line, i, 2)
        if (block > 0) {
          if (two == "/*") { block++; i += 2 }
          else if (two == "*/") { block--; i += 2; if (block == 0) out = out " " }
          else i++
          continue
        }
        if (str) {
          if (c == "\\") i += 2
          else if (c == "\"") { str = 0; out = out c; i++ }
          else i++
          continue
        }
        if (raw >= 0) {
          closing = "\""; for (k = 0; k < raw; k++) closing = closing "#"
          if (substr(line, i, length(closing)) == closing) {
            out = out closing; i += length(closing); raw = -1
          } else i++
          continue
        }
        if (two == "//") break
        if (two == "/*") { block = 1; i += 2; continue }
        if (c == "\"") { str = 1; out = out c; i++; continue }
        if ((c == "r" || ((c == "b" || c == "c") && substr(line, i + 1, 1) == "r")) \
            && (i == 1 || substr(line, i - 1, 1) !~ /[A-Za-z0-9_]/)) {
          j = i + (c == "r" ? 1 : 2); hashes = 0
          while (substr(line, j, 1) == "#") { hashes++; j++ }
          if (substr(line, j, 1) == "\"") {
            raw = hashes; out = out substr(line, i, j - i + 1); i = j + 1; continue
          }
        }
        if (c == "'\''") {
          # A character literal, escaped or not; anything else is a lifetime
          # or a label and stays code.
          if (substr(line, i + 1, 1) == "\\") {
            j = i + 3
            while (j <= n && substr(line, j, 1) != "'\''") j++
            out = out "'\'' '\''"; i = j + 1; continue
          }
          if (substr(line, i + 2, 1) == "'\''") { out = out "'\'' '\''"; i += 3; continue }
        }
        out = out c; i++
      }
      print FILENAME ":" FNR ":" out
    }
  ' "$@"
}

# The guard tests its own boundary: a word in a comment, a doc comment, a
# block comment, a string, a raw string or a character is gone; the code
# beside it stays, a lifetime included.
cat > "$probe_dir/literals.rs" <<'PROBE'
/// Never reach reqwest::Client directly.
let a = "reqwest::Client"; let b = reqwest::Client::new();
let c = r#"a "reqwest::" quote"#; /* reqwest:: /* nested */ reqwest:: */ let d = '"';
fn f<'a>(x: &'a str) -> char { '\'' } // reqwest::
let e = "multi
reqwest:: line"; let g = b"reqwest::";
PROBE
literal_probe=$(rust_code "$probe_dir/literals.rs" | sed 's|^.*/||; s/[[:space:]]*$//')
expected_literals='literals.rs:1:
literals.rs:2:let a = ""; let b = reqwest::Client::new();
literals.rs:3:let c = r#""#;   let d = '"'"' '"'"';
literals.rs:4:fn f<'"'"'a>(x: &'"'"'a str) -> char { '"'"' '"'"' }
literals.rs:5:let e = "
literals.rs:6:"; let g = b"";'
if [ "$literal_probe" != "$expected_literals" ]; then
  err "rust_code misreads its probe: got"
  printf '%s\n' "$literal_probe" >&2
fi

# --- 11a. Nothing outside iaam-http sends HTTP past the gateway ---
# The audit behind the gateway found callers that reached the transport
# directly and so skipped its budgets, lanes, retries and breaker without an
# error. The compiler now closes most of that: `HttpClient` cannot be built
# outside iaam-http (lib.rs pins it with compile_fail doctests), and the
# gateway never hands its transport out. What remains is what the compiler
# does not see: a `reqwest` client named by path, which guard 11 already
# refuses as a dependency but would compile through a transitive re-export,
# a client named by path, or `Transport::send` called on one. `.send(` alone
# is not looked for, because `Gateway::send` shares it. Every file is
# checked, tests and examples included: a live test that skips the budget
# spends the same real one.
bypass_hits() {
  rust_code $(find "$@" -name '*.rs' | sort) \
    | grep -E ':[0-9]+.*(HttpClient::send|Transport>::send|Transport::send[[:space:]]*\(|[^A-Za-z0-9_]reqwest::|HttpClient::(new|default)[[:space:]]*\(|:[[:space:]]*HttpClient[[:space:]]*=)' \
    || true
}
# The guard tests its own boundary: the gateway's own `send`, a string or a
# comment naming a client pass; a held client, a path call and a hand-built
# reqwest client do not.
mkdir -p "$probe_dir/bypass"
cat > "$probe_dir/bypass/allowed.rs" <<'PROBE'
let gateway = Arc::new(Gateway::production()?);
gateway.send("UsersService", &request, None).await
let message = "build it with reqwest::ClientBuilder"; // not reqwest::Client
PROBE
cat > "$probe_dir/bypass/bypass.rs" <<'PROBE'
let client = HttpClient::new();
Transport::send(&client, &request).await
let raw = reqwest::Client::new();
reqwest::get(url).await
PROBE
bypass_probe=$(bypass_hits "$probe_dir/bypass" | sed 's|^.*/||' | cut -d: -f1,2 | tr '\n' ' ')
if [ "$bypass_probe" != 'bypass.rs:1 bypass.rs:2 bypass.rs:3 bypass.rs:4 ' ]; then
  err "the gateway-bypass guard misclassifies its probe: got '$bypass_probe'"
fi
for crate_dir in crates/*/; do
  case "$crate_dir" in
    crates/iaam-http/) continue ;;
  esac
  hits=$(bypass_hits "$crate_dir")
  if [ -n "$hits" ]; then
    err "a request can be sent past the gateway in $crate_dir: outbound HTTP goes through Gateway::send (iaam-http module docs)"
    echo "$hits" >&2
  fi
done

# --- 11b. One gateway per process ---
# Budgets, lanes, named waits and breakers are per gateway, so a second
# gateway is a second allowance against the same destination and halves
# nothing (docs/deployment.md §1.1). A merge once left two of them in
# iaam-bootstrap. Production code — every `src` tree outside iaam-http, which
# defines the constructors — builds a gateway exactly once, with
# `Gateway::production()`, in `serve` of iaam-bootstrap, which shares it as
# an `Arc`. Every constructor counts: `production`, `new` and `with_parts`,
# however the type is spelled (`Gateway::<T>::new`, `<Gateway<T>>::new`).
# Tests, examples and live probes build their own; they are not the process.
#
# "Test code" is an item under `#[cfg(test)]` — a module, a function, an
# impl — and it ends where its braces close, not at the end of the file:
# production code placed after a test module is still production code, on the
# line the module closes on as well as on the lines after it.
#
# The limit: a path is read on one line. `Gateway ::` on one line and
# `production()` on the next is not chased, because `cargo fmt --check`, which
# CI runs, joins it back; an alias through a function item is caught only as
# the constructor named without a call, which is counted below.
production_code() {
  rust_code $(find "$@" -name '*.rs' | sort) | awk '
    {
      p = index($0, ":"); file = substr($0, 1, p - 1); rest = substr($0, p + 1)
      q = index(rest, ":"); line = substr(rest, 1, q - 1); code = substr(rest, q + 1)
      if (file != current) {
        current = file; depth = 0; nesting = 0; pending = 0; skip = 0; fns = 0; next_fn = ""
      }
      if (!skip && code ~ /^[[:space:]]*#\[cfg\(test\)\]/) {
        pending = 1; sub(/^[[:space:]]*#\[cfg\(test\)\][[:space:]]*/, "", code)
      }
      if (!skip && pending) {
        # Blank lines and further attributes keep the item ahead pending.
        if (code ~ /^[[:space:]]*$/ || code ~ /^[[:space:]]*#\[.*\][[:space:]]*$/) next
        skip = 1; skip_depth = depth; skip_opened = 0; pending = 0
      }
      in_test = skip; kept = ""
      # The function a line belongs to: the one open when it starts, or,
      # for a function written on one line, the one it declares.
      enclosing = fns > 0 ? fn_name[fns] : ""; declared = ""
      rest = code
      while (rest != "") {
        if (match(rest, /^fn[[:space:]]+[A-Za-z0-9_]+/) && (prev == "" || prev !~ /[A-Za-z0-9_]/)) {
          next_fn = substr(rest, 1, RLENGTH); sub(/^fn[[:space:]]+/, "", next_fn)
          if (!skip) kept = kept substr(rest, 1, RLENGTH)
          prev = "n"; rest = substr(rest, RLENGTH + 1); continue
        }
        c = substr(rest, 1, 1); rest = substr(rest, 2); prev = c
        # A character belongs to the test item while one is open, the brace
        # or semicolon that closes it included.
        if (!skip) kept = kept c
        if (c == "(" || c == "[") nesting++
        else if (c == ")" || c == "]") nesting--
        else if (c == "{") {
          depth++
          if (skip) skip_opened = 1
          if (next_fn != "") {
            fns++; fn_name[fns] = next_fn; fn_depth[fns] = depth
            # A function the test item declares is not the one kept code
            # after it belongs to.
            if (declared == "" && !skip) declared = next_fn
            next_fn = ""
          }
        } else if (c == "}") {
          depth--
          while (fns > 0 && depth < fn_depth[fns]) fns--
          if (skip && skip_opened && depth == skip_depth) skip = 0
        } else if (c == ";" && nesting == 0) {
          next_fn = ""
          if (skip && !skip_opened && depth == skip_depth) skip = 0
        }
      }
      prev = ""
      if (enclosing == "" || in_test) enclosing = declared
      if (!in_test || kept ~ /[^[:space:]]/) print file ":" line ":" enclosing ":" kept
    }
  '
}
# A constructor counts whether it is called, named as a value
# (`let build = Gateway::production;`) or imported (`use …::Gateway::new`,
# `use …::Gateway::{new, …}`): each is a way to build one more.
GATEWAY_CONSTRUCTOR='[^A-Za-z0-9_]Gateway(::)?(<[^;=()]*>)?>?::((production|new|with_parts)([^A-Za-z0-9_]|$)|\{([^}]*[,[:space:]])?(production|new|with_parts)[[:space:]]*[,}])'
gateway_constructions() {
  production_code "$@" | { grep -E "^[^:]*:[0-9]+:[^:]*.*$GATEWAY_CONSTRUCTOR" || true; }
}
# An alias is a constructor this guard cannot count: `use …::Gateway as G`
# then `G::new(…)`, or `type G = Gateway<T>` then `G::production()`. Outside
# iaam-http production code names the type as it is; a type that merely
# contains it (`Arc<Gateway<T>>`) builds nothing and passes.
gateway_aliases() {
  production_code "$@" | {
    grep -E '^[^:]*:[0-9]+:[^:]*.*([^A-Za-z0-9_]Gateway[[:space:]]+as[[:space:]]|[^A-Za-z0-9_]type[[:space:]]+[A-Za-z0-9_]+[[:space:]]*(<[^=]*>)?[[:space:]]*=[[:space:]]*([A-Za-z0-9_]+::)*Gateway[[:space:]]*(<|;|$))' \
      || true
  }
}
# The guard tests its own boundary on an invented crate: the one permitted
# construction; a second one after the test module; one inside a
# `#[cfg(test)]` function and one inside the test module; constructors
# spelled with turbofish and qualified paths; a string and a comment naming
# a constructor; aliases that would hide one, and a type that only contains
# the gateway.
mkdir -p "$probe_dir/gateway"
cat > "$probe_dir/gateway/main.rs" <<'PROBE'
async fn serve(config: Config) -> Result<(), Error> {
    let gateway = Arc::new(Gateway::production()?);
    let note = "Gateway::with_parts( is for tests"; // Gateway::new(
    let held: Arc<Gateway<HttpClient>> = Arc::new(build());
}

#[cfg(test)]
#[inline]
fn fake() -> Gateway<Fake> {
    Gateway::with_parts(Fake, clock, sleeper)
}

#[cfg(test)]
mod tests {
    fn build() {
        let inner = { Gateway::new(Fake) };
    }
}

fn after_the_tests() {
    let second = Gateway::<HttpClient>::production();
}

impl Holder {
    fn third() -> Self { Self(<Gateway<Fake>>::with_parts(a, b, c)) }
}

#[cfg(test)] mod inline { fn t() { Gateway::new(Fake); } } fn fourth() { Gateway::production(); }

fn named_not_called() {
    let build = Gateway::production;
    let also = Gateway::<HttpClient>::new;
}

use iaam_http::Gateway::production;
use iaam_http::Gateway::{new, with_parts};
PROBE
cat > "$probe_dir/gateway/alias.rs" <<'PROBE'
use iaam_http::{Gateway as Front, Outbound};
type Direct = iaam_http::Gateway<HttpClient>;
type Shared = Arc<Gateway<HttpClient>>;
fn wrapped(gateway: Arc<Gateway<HttpClient>>) -> Shared { gateway }
PROBE
constructions_probe=$(gateway_constructions "$probe_dir/gateway" | sed 's|^.*/||' | cut -d: -f1-3 | tr '\n' ' ')
if [ "$constructions_probe" != 'main.rs:2:serve main.rs:21:after_the_tests main.rs:25:third main.rs:28:fourth main.rs:31:named_not_called main.rs:32:named_not_called main.rs:35: main.rs:36: ' ]; then
  err "the one-gateway guard misclassifies its probe: got '$constructions_probe'"
fi
aliases_probe=$(gateway_aliases "$probe_dir/gateway" | sed 's|^.*/||' | cut -d: -f1,2 | tr '\n' ' ')
if [ "$aliases_probe" != 'alias.rs:1 alias.rs:2 ' ]; then
  err "the gateway-alias guard misclassifies its probe: got '$aliases_probe'"
fi

production_src=()
for crate_src in crates/*/src; do
  case "$crate_src" in
    crates/iaam-http/src) continue ;;
  esac
  [ -d "$crate_src" ] && production_src+=("$crate_src")
done
found=$(gateway_constructions "${production_src[@]}")
# Constructions, not lines: `(Gateway::production()?, Gateway::new(t)?)` is one
# line that rustfmt keeps as it is, and two gateways.
count=$(printf '%s' "$found" | { grep -oE "$GATEWAY_CONSTRUCTOR" || true; } | { grep -c . || true; })
located=$(printf '%s' "$found" | cut -d: -f1,3)
# Captured text is compared, never piped into `grep -q`: an early exit of
# grep under pipefail reads as "no match" (see guard 4).
if [ "$count" -ne 1 ] || [ "$located" != "crates/iaam-bootstrap/src/main.rs:serve" ] \
    || [[ "$found" != *"Gateway::production("* ]]; then
  err "production code must build a gateway exactly once, with Gateway::production() in serve of iaam-bootstrap; found $count:"
  printf '%s\n' "$found" >&2
fi
hits=$(gateway_aliases "${production_src[@]}")
if [ -n "$hits" ]; then
  err "production code renames the gateway type, which hides a construction from the one-gateway guard:"
  printf '%s\n' "$hits" >&2
fi

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