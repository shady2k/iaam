//! The document an arriving client reads first (RFC 9727, RFC 9264).
//!
//! An agent that has just been handed a token and a host knows nothing about
//! this instance. What it finds at `/v1/openapi.json` is sixty-six routes, all
//! described equally well, with nothing among them saying which are the
//! questions this API exists to answer, which one says what to do next, and
//! which of the rest exist to be linked to rather than called cold. The catalog
//! is where that ordering is stated, because it is the one address the agent
//! reaches without being told anything.
//!
//! So the document names three things beyond the contract and the health
//! resource:
//!
//! - **the four goals**, taken from [`ReportGoal`] rather than spelled here —
//!   the code, the line saying what each answers, and the route that answers it.
//!   The line was written here and moved to the goal itself when the
//!   outstanding-work queue began publishing the same four names to be read out
//!   (`iaam-i3nx`); two descriptions of one report, published by one system, is
//!   the divergence [`ReportGoal::code`] is in the core to prevent. Those names
//!   are published by the outstanding-work queue on every item and by every
//!   report's confidence register, so a client holding a caveat that says
//!   `money_flow` can ask this document which route that is;
//! - **the queue**, which is the answer to "what should I do first" and the only
//!   route that reads this instance's state to produce it;
//! - **the scopes**, because every one of the four goal routes requires a scope
//!   id and nothing in this API accepts a scope by name. A cold client that went
//!   straight from the catalog to a report would be missing exactly one value,
//!   and this is where it comes from.
//!
//! # Why it is built from the generated document, not written out
//!
//! The previous catalog was a `const` byte string holding two hrefs. It was
//! correct, and it was correct only because nobody had renamed a route: a path
//! typed into a string literal is a claim about the router that the router does
//! not check. The entry point advertising a dead link is the worst failure this
//! document has, because it is read by clients that have nothing else to go on.
//!
//! Every address here is therefore resolved from the completed OpenAPI document
//! by `operation_id`, exactly as [`crate::ActionCatalog`] resolves the addresses
//! it publishes in queue items, and for the same reason. A renamed path moves
//! with the handler; a removed or non-`GET` operation refuses the build.
//!
//! What resolution does **not** remove is the mapping — which operation answers
//! which goal is a judgement, and no amount of introspection derives it. It is
//! written once, as an exhaustive `match` on [`ReportGoal`] in
//! [`answering_operation`], so a fifth goal would be a compile error rather than
//! a goal quietly missing from the catalog.
//!
//! The cost is that `build` can fail. It is the cost already accepted for
//! [`crate::ActionCatalog`], and it is smaller than it looks: the document is
//! generated from the same binary, so this resolution cannot fail for anything
//! environmental. It fails on the first run after a mistake, everywhere, which
//! is a compile-time check wearing a runtime coat.

use std::collections::BTreeMap;

use axum::body::Bytes;
use serde_json::{Map, Value, json};
use thiserror::Error;
use utoipa::openapi::OpenApi;

use iaam_core::goal::ReportGoal;

/// The address of the machine-readable contract.
///
/// The one address in this file not resolved from the generated document, and
/// it cannot be: `/v1/openapi.json` is mounted with a plain `Router::route`
/// after `split_for_parts`, so the document does not describe itself. It is
/// spelled once, here, and `crate::build` mounts the route from this constant —
/// which is the only arrangement in which the catalog and the router cannot
/// disagree about it.
pub const OPENAPI_PATH: &str = "/v1/openapi.json";

/// The context every link in this document hangs off.
const ANCHOR: &str = "/v1";

/// The media type of everything linked here.
const JSON: &str = "application/json";

/// The operation that reports whether the instance is answering.
const STATUS_OPERATION: &str = "health";

/// The operation that computes what this instance needs next.
const QUEUE_OPERATION: &str = "list_actions";

/// The operation that lists the scopes a report can be computed over.
const SCOPES_OPERATION: &str = "list_contours";

/// The operation that answers a goal.
///
/// A judgement, and the only hand-written part of the catalog that resolution
/// cannot check: nothing in the router says which route is `money_flow`. An
/// exhaustive `match` rather than a table, so that a fifth goal cannot be added
/// without answering this question for it.
///
/// `asset_snapshot` is answered by two routes — the snapshot and the per-account
/// balances both publish that goal in their confidence register. The catalog
/// names one, the one whose answer is the whole holding at a date; the other is
/// in the contract, a client that wants it reads it there. Publishing both would
/// hand a cold client a choice it has no basis to make.
const fn answering_operation(goal: ReportGoal) -> &'static str {
    match goal {
        ReportGoal::AssetSnapshot => "asset_snapshot_report",
        ReportGoal::MoneyFlow => "flow_report",
        ReportGoal::Returns => "returns_report",
        ReportGoal::Reconciliation => "reconciliation",
    }
}

/// The discovery document, serialised once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCatalog {
    body: Bytes,
}

/// A failure found while resolving the catalog's links against OpenAPI.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiCatalogError {
    #[error("catalog link {operation_id} resolves to no GET operation in the generated document")]
    MissingOperation { operation_id: String },
    #[error("operation_id {operation_id} is declared on more than one GET operation")]
    DuplicateOperationId { operation_id: String },
}

impl ApiCatalog {
    /// Resolve every link against a completed OpenAPI document.
    ///
    /// # Errors
    ///
    /// [`ApiCatalogError::MissingOperation`] when a linked operation is absent
    /// or has stopped being a `GET`; a catalog is a list of things to read, and
    /// an href a client cannot `GET` is the dead link this whole arrangement
    /// exists to make impossible.
    pub fn from_openapi(api: &OpenApi) -> Result<Self, ApiCatalogError> {
        let by_id = get_operations(api)?;
        let resolve = |operation_id: &'static str| -> Result<&str, ApiCatalogError> {
            by_id.get(operation_id).map(String::as_str).ok_or_else(|| {
                ApiCatalogError::MissingOperation {
                    operation_id: operation_id.to_owned(),
                }
            })
        };

        let mut related = vec![
            link(
                resolve(QUEUE_OPERATION)?,
                "What this instance needs next, computed from its own state. Each item names \
                 the operation that closes it, the fields already decided, and the goals below \
                 it stands in the way of. Work this queue rather than reconstructing an order \
                 of setup.",
                None,
            ),
            link(
                resolve(SCOPES_OPERATION)?,
                "The scopes a report is computed over, each with the owner's own name for it. \
                 Every goal below requires one of these ids: read the scope by name here, then \
                 call by id.",
                None,
            ),
        ];
        for goal in ReportGoal::ALL {
            related.push(link(
                resolve(answering_operation(goal))?,
                goal.answers(),
                Some(goal.code()),
            ));
        }

        let document = json!({
            "linkset": [{
                "anchor": ANCHOR,
                "service-desc": [link(
                    OPENAPI_PATH,
                    "The contract: every route, request, response and refusal this instance \
                     serves, generated from its handlers. Read it instead of asking which \
                     routes exist or what a refusal looks like.",
                    None,
                )],
                "status": [link(
                    resolve(STATUS_OPERATION)?,
                    "Whether the instance is answering, and the schema and projection versions \
                     it answers under.",
                    None,
                )],
                "related": related,
            }]
        });

        Ok(Self {
            body: Bytes::from(
                serde_json::to_vec(&document).expect("the catalog is built from owned strings"),
            ),
        })
    }

    /// The serialised document. Cheap to clone: the bytes are shared.
    #[must_use]
    pub fn body(&self) -> Bytes {
        self.body.clone()
    }
}

/// One link, in the JSON serialisation of a linkset (RFC 9264 §4.2).
///
/// `goal` is an **extension target attribute**, which the serialisation allows
/// beside the reserved ones. The alternative was an extension link relation
/// type, and RFC 8288 requires those to be absolute URIs — a namespace under a
/// domain this project does not own, invented to say a word the payload can
/// carry as an attribute. So the four report links sit under the registered
/// `related` relation and are told apart from the other two by carrying a
/// `goal`, whose value is exactly what the queue and every report's confidence
/// register publish.
fn link(href: &str, title: &str, goal: Option<&str>) -> Value {
    let mut object = Map::new();
    object.insert("href".to_owned(), Value::String(href.to_owned()));
    object.insert("type".to_owned(), Value::String(JSON.to_owned()));
    object.insert("title".to_owned(), Value::String(title.to_owned()));
    if let Some(goal) = goal {
        object.insert("goal".to_owned(), Value::String(goal.to_owned()));
    }
    Value::Object(object)
}

/// Every `GET` operation in the document, by `operation_id`.
///
/// `GET`-only on purpose. An operation that stops being a `GET` stops being
/// something a link can address, and it disappears from this index rather than
/// resolving to an href a client cannot follow.
fn get_operations(api: &OpenApi) -> Result<BTreeMap<String, String>, ApiCatalogError> {
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    for (path, item) in &api.paths.paths {
        let Some(operation) = item.get.as_ref() else {
            continue;
        };
        let Some(operation_id) = operation.operation_id.clone() else {
            // An operation with no id is refused by `ActionCatalog::from_openapi`
            // over the whole document, with a message naming the method and the
            // path. Repeating that here would only give the same defect a second
            // spelling.
            continue;
        };
        if by_id.insert(operation_id.clone(), path.clone()).is_some() {
            return Err(ApiCatalogError::DuplicateOperationId { operation_id });
        }
    }
    Ok(by_id)
}
