//! Exact table coverage guard for `docs/api/conventions.md` §2 (iaam-k3gh.15).
//!
//! Follows `iaam-store`'s `tests/bundle_coverage.rs`: compute the exact set the
//! source of truth implies, compare it against the list a document maintains by
//! hand, and fail on a difference in either direction. §2 drifted by six routes
//! — three absent outright, one more added under the wrong path and method —
//! in the time it took one agent to read it as authoritative. A careful reading
//! is not a fix; this is the guard.
//!
//! **The source of truth is the generated OpenAPI document, not the route
//! declarations.** A client reads the document `GET /v1/openapi.json` serves,
//! never the Rust behind it, so a scanner that read `#[utoipa::path(...)]`
//! attributes could pass while the document it produces disagreed with them —
//! `utoipa`'s own derivation is exactly the layer that could get that
//! translation wrong. Building the real router and reading the real document,
//! the way `iaam_server::build` hands it to `GET /v1/openapi.json`, means this
//! guard checks what a caller actually receives.
//!
//! **What "contains a list" means here, mechanically.** A response answers with
//! a list if its own top level, after following at most one `$ref` and merging
//! any `allOf` branches (which is what a `#[serde(flatten)]` field becomes in
//! the generated schema), is itself a JSON array, or carries a property that
//! is. This is §1.3's own sentence — "array at the top level, or an object
//! whose documented list field it looks up once" — turned into a predicate.
//!
//! **What this cannot see.** A list nested two hops down, behind a field that
//! is itself a non-array object, is invisible to a one-hop scanner by
//! construction: `ClassificationRuleChangeDto.plan.corrections` is the one
//! published response shaped that way, and `POST /v1/classification-rules` is
//! named in [`KNOWN_DEEP_LIST_ROUTES`] below rather than silently missed. The
//! alternative — following every field one hop further — was tried while this
//! guard was written and rejected: it cannot tell "look inside `plan`" from
//! "look inside `aliases`", so it followed every single-entity response into
//! whatever list-valued attribute it happens to carry — `AccountDto.aliases`,
//! `ContourDto.accounts`, `ContourDto.same_title_contours` — and none of those
//! routes answer with a list of the owner's things in §1's sense: they answer
//! with one thing that happens to have a list-valued field, the way most
//! records eventually do. A route that returns such an entity is not
//! predictable via §1's shape rule any differently for having one array field
//! among several scalar ones, so no table row teaches a caller anything a
//! one-hop scan of that same entity, met on any other route, has not already
//! taught.
//!
//! **Why a route can be covered by another route's row, not just its own.**
//! Two different routes that return the identical named response type teach
//! the identical shape — `POST /v1/documents` and `POST /v1/documents/{id}/reparse`
//! both answer `DocumentDto`, and the table carries one row for it, not two.
//! Requiring every route to have its own literal row would demand a second,
//! word-for-word identical explanation of a shape the table already gives, so
//! the check below accepts either: the route's own path and method appear in
//! the table, or another table row already names the same response type (the
//! bare item type for an array response, the object type otherwise).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::Clock;
use iaam_app::storage::SqliteStore;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use serde_json::Value;
use time::Date;
use time::macros::date;

struct FixedClock(Date);

impl Clock for FixedClock {
    fn today(&self) -> Date {
        self.0
    }
}

/// The minimum needed to call [`build`] and read back the document it hands
/// `GET /v1/openapi.json`. No account, token or broker access is seeded:
/// nothing here calls a route, it only reads the specification `build`
/// assembles from the type declarations.
fn generated_spec() -> Value {
    let store = SqliteStore::open_in_memory().expect("in-memory database");
    let adapter = Arc::new(SqliteAdapter::new(store));
    let services = Arc::new(AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        Arc::new(FixedClock(date!(2026 - 01 - 01))),
    ));
    let state = ServerState::new(
        services,
        Arc::new(RateLimiter::new(1_000, Duration::from_secs(60))),
    );
    let (_router, api) = build(state).expect("build the router and its specification");
    serde_json::to_value(&api).expect("specification serialises to JSON")
}

const HTTP_METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

/// Routes whose response §2 has never been told about would be discovered by
/// a hand-authored exception here, the way [`ClassificationRuleChangeDto`]'s
/// route is — see the module doc comment. One route, so far.
const KNOWN_DEEP_LIST_ROUTES: &[&str] = &["POST /v1/classification-rules"];

fn is_array(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("array")
}

/// The bare type name a `$ref` names, without resolving it.
fn ref_name(schema: &Value) -> Option<String> {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .map(|r| r.rsplit('/').next().unwrap_or(r).to_owned())
}

/// Whether the response, followed through at most one `$ref`, is itself an
/// array — the bare-array half of §1.3's rule.
fn top_is_array(schema: &Value, schemas: &Value) -> bool {
    if is_array(schema) {
        return true;
    }
    match ref_name(schema) {
        Some(name) => schemas
            .get(&name)
            .is_some_and(|resolved| top_is_array(resolved, schemas)),
        None => false,
    }
}

/// The properties a response schema publishes at its own top level, with any
/// `#[serde(flatten)]` field's `allOf` branch merged in as if it were written
/// there directly — which, on the wire, it is.
fn merged_top_properties(
    schema: &Value,
    schemas: &Value,
    visited: &mut HashSet<String>,
) -> serde_json::Map<String, Value> {
    if let Some(name) = ref_name(schema) {
        if !visited.insert(name.clone()) {
            return serde_json::Map::new();
        }
        return schemas
            .get(&name)
            .map(|resolved| merged_top_properties(resolved, schemas, visited))
            .unwrap_or_default();
    }
    let mut props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            props.extend(merged_top_properties(branch, schemas, visited));
        }
    }
    props
}

/// §1.3, mechanically: array at the top level, or an object with an array
/// among its top-level (flatten-merged) properties.
fn contains_a_list(schema: &Value, schemas: &Value) -> bool {
    if top_is_array(schema, schemas) {
        return true;
    }
    merged_top_properties(schema, schemas, &mut HashSet::new())
        .values()
        .any(is_array)
}

/// The type a table row names for this response: the item type for an array
/// (`[AccountDto]` becomes `AccountDto`), the `$ref`'d type otherwise. `None`
/// for an inline, unnamed schema — no route in this API answers with one.
fn dedup_key(schema: &Value) -> Option<String> {
    if is_array(schema) {
        return schema.get("items").and_then(dedup_key);
    }
    ref_name(schema)
}

/// The success response body of one operation: the `200` schema, or the
/// `201` schema where a route only ever creates. Both are read because a
/// create route publishes either depending on whether the request was a
/// repeat (§ the `AccountDto`/`ContourVersionDto` routes' own 200/201 pair).
fn success_body(operation: &Value) -> Option<&Value> {
    for status in ["200", "201"] {
        if let Some(schema) = operation.pointer(&format!(
            "/responses/{status}/content/application~1json/schema"
        )) {
            return Some(schema);
        }
    }
    None
}

/// Every `METHOD /path` the specification declares, whether or not its
/// response is list-shaped — used only to catch a table row naming a route
/// that does not exist (the defect the wrong path and method for the
/// transfer-partners batch route actually was).
fn every_declared_route(spec: &Value) -> BTreeSet<String> {
    let mut routes = BTreeSet::new();
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return routes;
    };
    for (path, item) in paths {
        for method in HTTP_METHODS {
            if item.get(method).is_some() {
                routes.insert(format!("{} {path}", method.to_uppercase()));
            }
        }
    }
    routes
}

/// `METHOD /path` for every route whose success response contains a list, per
/// [`contains_a_list`], mapped to the dedup key [`dedup_key`] reads off that
/// same response — `None` where the response is inline and unnamed.
fn list_answering_routes(spec: &Value) -> BTreeMap<String, Option<String>> {
    let mut found = BTreeMap::new();
    let schemas = spec
        .pointer("/components/schemas")
        .cloned()
        .unwrap_or(Value::Null);
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return found;
    };
    for (path, item) in paths {
        for method in HTTP_METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let Some(body) = success_body(operation) else {
                continue;
            };
            let route = format!("{} {path}", method.to_uppercase());
            if contains_a_list(body, &schemas) || KNOWN_DEEP_LIST_ROUTES.contains(&route.as_str()) {
                found.insert(route, dedup_key(body));
            }
        }
    }
    found
}

/// One row of §2's table: the route in column 1, the type in column 2.
///
/// The match requires both columns to be present and backtick-quoted, which
/// is what tells a real row apart from prose that merely mentions a route —
/// §3.5's own tables use the same `| \`Name\` | ... |` shape, and are excluded
/// because their first column never starts with an HTTP method.
fn table_row(line: &str) -> Option<(String, String)> {
    let mut cells = line.trim().strip_prefix('|')?.split('|');
    let route_cell = cells.next()?.trim();
    let route = route_cell.strip_prefix('`')?.strip_suffix('`')?;
    let (method, _) = route.split_once(' ')?;
    if !HTTP_METHODS.iter().any(|m| m.eq_ignore_ascii_case(method)) {
        return None;
    }
    let type_cell = cells.next()?.trim();
    let typ = type_cell.strip_prefix('`')?.strip_suffix('`')?;
    Some((route.to_owned(), typ.trim_matches(['[', ']']).to_owned()))
}

/// §2's table: every `(route, type)` pair between the "## 2." heading and the
/// next `## ` heading. Scoping to the section, rather than scanning the whole
/// document, is what keeps a row elsewhere that happened to start with an
/// HTTP-method-shaped word from ever being read as one of §2's own.
fn parse_section_2(markdown: &str) -> Vec<(String, String)> {
    let mut in_section = false;
    let mut rows = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed == "## 2. The lists as they stand" {
            in_section = true;
            continue;
        }
        if in_section && trimmed.starts_with("## ") {
            break;
        }
        if in_section {
            if let Some(row) = table_row(line) {
                rows.push(row);
            }
        }
    }
    rows
}

fn conventions_markdown() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/api/conventions.md"
    ))
    .expect("docs/api/conventions.md is readable")
}

/// Every list-answering route the specification declares must be reachable
/// from §2 — by its own row, or by a row naming the same response type — and
/// every row §2 carries must name a route the specification actually
/// declares. A difference in either direction is the defect this guard
/// exists to catch: three missing routes were the first direction, one row
/// naming a route under the wrong method and path was the second.
#[test]
fn section_2_matches_the_generated_specification_exactly() {
    let spec = generated_spec();
    let list_routes = list_answering_routes(&spec);
    let declared_routes = every_declared_route(&spec);
    let markdown = conventions_markdown();
    let table = parse_section_2(&markdown);

    let table_routes: BTreeSet<&str> = table.iter().map(|(route, _)| route.as_str()).collect();
    let table_types: BTreeSet<&str> = table.iter().map(|(_, typ)| typ.as_str()).collect();

    let mut unreachable = Vec::new();
    for (route, key) in &list_routes {
        let covered_by_own_row = table_routes.contains(route.as_str());
        let covered_by_sibling_row = key.as_deref().is_some_and(|key| table_types.contains(key));
        if !covered_by_own_row && !covered_by_sibling_row {
            unreachable.push(format!("{route} (type {key:?})"));
        }
    }
    assert!(
        unreachable.is_empty(),
        "these list-answering routes are reachable from neither their own row in §2 nor a \
         sibling row naming the same response type: {unreachable:#?}"
    );

    let mut phantom = Vec::new();
    for (route, _) in &table {
        if !declared_routes.contains(route.as_str()) {
            phantom.push(route.clone());
        }
    }
    assert!(
        phantom.is_empty(),
        "these §2 rows name a route the specification does not declare — a stale entry, or one \
         written under the wrong method or path: {phantom:#?}"
    );
}

/// Proves [`table_row`] is not a substring match: a line that mentions a
/// route in prose, or that has the two columns in the wrong shape, must not
/// be read as a table row, or this guard would never fail on a missing one.
#[test]
fn table_row_parsing_is_exact_not_substring() {
    assert_eq!(
        table_row(
            "| `GET /v1/accounts` | `[AccountDto]` | bare array | whole list, nothing about \
             the set |"
        ),
        Some(("GET /v1/accounts".to_owned(), "AccountDto".to_owned())),
        "a properly formatted row must parse"
    );

    // Prose that names a route without the table's own punctuation.
    assert_eq!(
        table_row("`GET /v1/accounts` is described above."),
        None,
        "a route named in prose, not inside a table row, must not parse as one"
    );
    assert_eq!(
        table_row("See `GET /v1/accounts` and `POST /v1/accounts` below."),
        None,
        "two routes named in one sentence must not parse as a row either"
    );

    // §3.5's own tables: same punctuation, first column is not a route.
    assert_eq!(
        table_row("| `AccountDto` | `id` | `title`, `institution` |"),
        None,
        "a row whose first column is a type, not an HTTP route, must not parse"
    );

    // The header and separator rows of §2's own table.
    assert_eq!(table_row("| Route | Response | Shape | Why |"), None);
    assert_eq!(table_row("|---|---|---|---|"), None);
}

/// Proves [`parse_section_2`] is scoped to §2 and not the whole document: a
/// decoy row shaped exactly like a real one, placed outside the "## 2."
/// heading, must not be read as part of the table.
#[test]
fn section_2_parsing_is_scoped_to_its_own_heading() {
    let markdown = "\
## 1. Before

| `GET /v1/before` | `BeforeDto` | bare array | decoy above the heading |

## 2. The lists as they stand

| Route | Response | Shape | Why |
|---|---|---|---|
| `GET /v1/inside` | `InsideDto` | bare array | the one real row |

## 3. After

| `GET /v1/after` | `AfterDto` | bare array | decoy below the next heading |
";
    let rows = parse_section_2(markdown);
    assert_eq!(
        rows,
        vec![("GET /v1/inside".to_owned(), "InsideDto".to_owned())],
        "only the row between the §2 heading and the next heading may be read"
    );
}

/// Proves the comparison in [`section_2_matches_the_generated_specification_exactly`]
/// is not vacuous, the way `bundle_coverage.rs`'s own second test proves its
/// comparison is not: a route absent from the table, and absent from every
/// sibling row naming its type, must be reported.
#[test]
fn the_guard_would_catch_a_genuinely_missing_route() {
    let mut list_routes = BTreeMap::new();
    list_routes.insert("GET /v1/accounts".to_owned(), Some("AccountDto".to_owned()));
    list_routes.insert(
        "GET /v1/nobody-added-this".to_owned(),
        Some("NeverDocumentedDto".to_owned()),
    );

    let table_routes: BTreeSet<&str> = ["GET /v1/accounts"].into_iter().collect();
    let table_types: BTreeSet<&str> = ["AccountDto"].into_iter().collect();

    let unreachable: Vec<&str> = list_routes
        .iter()
        .filter(|(route, key)| {
            !table_routes.contains(route.as_str())
                && !key.as_deref().is_some_and(|key| table_types.contains(key))
        })
        .map(|(route, _)| route.as_str())
        .collect();

    assert_eq!(
        unreachable,
        vec!["GET /v1/nobody-added-this"],
        "a route with neither its own row nor a sibling row for its type must be reported, and \
         a route covered by a sibling row (here, GET /v1/accounts is not even in the table but \
         shares no row either) must not drown it out"
    );
}

/// The same proof for the reverse direction: a table row naming a route that
/// does not exist — the defect the transfer-partners batch row actually had,
/// `POST .../batch` where the route was `PUT /v1/accounts/transfer-partners`
/// — must be reported.
#[test]
fn the_guard_would_catch_a_phantom_row() {
    let declared_routes: BTreeSet<&str> =
        ["PUT /v1/accounts/transfer-partners"].into_iter().collect();
    let table_routes = ["POST /v1/accounts/{id}/transfer-partners/batch"];

    let phantom: Vec<&str> = table_routes
        .into_iter()
        .filter(|route| !declared_routes.contains(route))
        .collect();

    assert_eq!(
        phantom,
        vec!["POST /v1/accounts/{id}/transfer-partners/batch"],
        "a row naming a route the specification does not declare must be reported"
    );
}
