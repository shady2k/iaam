use std::collections::BTreeMap;

use thiserror::Error;
use utoipa::openapi::{OpenApi, RefOr, path::Operation};

use iaam_app::actions::OperationKey;
use iaam_app::ports::{Scope, required_scope};
use iaam_core::goal::ReportGoal;

use crate::api_catalog::answering_operation;

/// A route address resolved from the completed OpenAPI document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOperation {
    pub operation_id: String,
    pub method: String,
    pub path: String,
    /// The component schema the route's JSON body answers to, where it takes
    /// one.
    ///
    /// `Option`, and the emptiness is the honest answer rather than a
    /// concession: `POST /v1/import-sessions/{session}/abandon` takes no body
    /// at all — the session is in the path and there is nothing to say about
    /// abandoning it — so it has no request schema, and demanding one would
    /// have meant either refusing to publish a call that exists or inventing a
    /// body for it so that a catalogue would accept it. The route was the first
    /// to need addressing when a refusal began offering it as a way out.
    pub request_schema: Option<String>,
    /// The narrowest token scope the route lets through to this call.
    ///
    /// **Not resolved from the document, and it is the only field here that is
    /// not.** The completed contract states a method, a path and a request
    /// schema, and this catalogue reads all three off it; it does not state an
    /// authority. Every route declares the same `security(("bearer" = []))`,
    /// and the prose beside the 403 already disagrees with the handlers.
    /// So the floor is taken from `iaam_app::ports`, which is the same module
    /// `iaam_server::routes` gates a write route by — see [`required_scope`] for
    /// why the fact is not written into the document instead.
    ///
    /// Two entries take it two ways, and both are that module's statement. An
    /// [`OperationKey`] is answered by [`required_scope`], which is total over
    /// the vocabulary. A report answer is [`Scope::ReadOnly`], the floor that
    /// module defines as the one every token is admitted to: a report is a read,
    /// it deliberately has no key, and no write authority stands in front of it.
    ///
    /// It is carried here rather than looked up at each use for the reason the
    /// address is: an action's target, a caveat's remedy, a refusal's way out
    /// and a standing's answer are all built from an [`ActionOperation`], and a
    /// second lookup beside one of them is a second answer.
    pub required_scope: Scope,
}

/// The operation addresses this transport publishes.
///
/// Two readers, holding two kinds of call. A computed action, a caveat's remedy
/// and a refusal's way out name an [`OperationKey`], which is a call that
/// changes something and therefore resolves against the whole vocabulary. A
/// report standing names the call that answers its goal, which is a read: it has
/// no key, and it resolves through the one goal-to-route mapping the catalog
/// document also publishes — [`answering_operation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionCatalog {
    operations: BTreeMap<&'static str, ActionOperation>,
    /// The call that produces each goal's report, by goal.
    ///
    /// A map rather than four fields because the four are read by iterating the
    /// vocabulary: a goal added to [`ReportGoal`] is resolved here or the build
    /// fails, exactly as a twenty-first [`OperationKey`] is.
    answers: BTreeMap<ReportGoal, ActionOperation>,
}

/// A failure found while resolving action references against OpenAPI.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ActionCatalogError {
    #[error("operation {method} {path} has no operation_id")]
    MissingOperationId { method: String, path: String },
    #[error("action operation {operation_id} does not resolve to an OpenAPI operation")]
    MissingActionOperation { operation_id: String },
    #[error("operation_id {operation_id} is declared more than once")]
    DuplicateOperationId { operation_id: String },
    #[error("report answer {operation_id} does not resolve to an OpenAPI GET operation")]
    MissingAnswerOperation { operation_id: String },
}

impl ActionCatalog {
    /// Resolve every action operation against a completed OpenAPI document.
    pub fn from_openapi(api: &OpenApi) -> Result<Self, ActionCatalogError> {
        let mut by_id = BTreeMap::new();
        for (path, item) in &api.paths.paths {
            for (method, operation) in [
                ("GET", item.get.as_ref()),
                ("PUT", item.put.as_ref()),
                ("POST", item.post.as_ref()),
                ("DELETE", item.delete.as_ref()),
                ("OPTIONS", item.options.as_ref()),
                ("HEAD", item.head.as_ref()),
                ("PATCH", item.patch.as_ref()),
                ("TRACE", item.trace.as_ref()),
            ] {
                let Some(operation) = operation else {
                    continue;
                };
                let operation_id = operation.operation_id.clone().ok_or_else(|| {
                    ActionCatalogError::MissingOperationId {
                        method: method.to_owned(),
                        path: path.clone(),
                    }
                })?;
                if by_id
                    .insert(
                        operation_id.clone(),
                        (path.clone(), method.to_owned(), operation),
                    )
                    .is_some()
                {
                    return Err(ActionCatalogError::DuplicateOperationId { operation_id });
                }
            }
        }

        // The whole vocabulary, not a list repeated here. A key left out of a
        // hand-written list resolves to nothing, and [`Self::operation`] would
        // find that out at the moment a caller asked for it rather than at
        // start-up — which now matters to more than the queue: a caveat names
        // its remedy by the same key, and a report pointing at an unresolvable
        // call is exactly the drift this catalogue exists to refuse.
        let mut operations = BTreeMap::new();
        for key in OperationKey::ALL {
            let operation_id = key.as_str();
            let Some((path, method, operation)) = by_id.get(operation_id) else {
                return Err(ActionCatalogError::MissingActionOperation {
                    operation_id: operation_id.to_owned(),
                });
            };
            operations.insert(
                operation_id,
                ActionOperation {
                    operation_id: operation_id.to_owned(),
                    method: method.clone(),
                    path: path.clone(),
                    request_schema: request_schema(operation),
                    required_scope: required_scope(key),
                },
            );
        }

        // The four reports, through the one mapping that decides which route
        // answers which goal. Not read from `OperationKey`, which deliberately
        // holds only calls that change something: a report is a read, so its
        // address cannot come from the loop above and arriving here through the
        // catalogue is the only way it reaches a caller. The mapping is not
        // written out again to get it — [`answering_operation`] is the same
        // function the catalog document links each goal by, and the build fails
        // on a goal whose route has gone, exactly as it does for a key.
        let mut answers = BTreeMap::new();
        for goal in ReportGoal::ALL {
            let operation_id = answering_operation(goal);
            let Some((path, method, operation)) = by_id
                .get(operation_id)
                .filter(|resolved| resolved.1 == "GET")
            else {
                return Err(ActionCatalogError::MissingAnswerOperation {
                    operation_id: operation_id.to_owned(),
                });
            };
            answers.insert(
                goal,
                ActionOperation {
                    operation_id: operation_id.to_owned(),
                    method: method.clone(),
                    path: path.clone(),
                    request_schema: request_schema(operation),
                    // A report takes its contour and its interval as query
                    // parameters and a body from nobody. Nothing stands between
                    // a token and it, which is the floor `iaam_app::ports`
                    // defines as reachable by every token.
                    required_scope: Scope::ReadOnly,
                },
            );
        }

        Ok(Self { operations, answers })
    }

    /// Return the route address for an operation.
    ///
    /// Total, and it is [`Self::from_openapi`] that makes it so: every
    /// [`OperationKey`] is registered or the build fails, so a key that reaches
    /// here has an address. The index is deliberate — a lookup returning
    /// `Option` would invite a caller to publish an item with the address
    /// silently missing.
    #[must_use]
    pub fn operation(&self, key: OperationKey) -> &ActionOperation {
        &self.operations[key.as_str()]
    }

    /// Return the route address of the call that produces a goal's report.
    ///
    /// Total for the reason [`Self::operation`] is, and made so by the same
    /// build: the four goals are resolved at start-up or the server does not
    /// start, so a standing that reaches here has an address rather than a
    /// field to publish as absent. It is a lookup rather than an `Option` for
    /// the same reason too — a caller publishing an address a client cannot
    /// follow is the failure this catalogue exists to refuse.
    #[must_use]
    pub fn answering(&self, goal: ReportGoal) -> &ActionOperation {
        &self.answers[&goal]
    }
}

fn request_schema(operation: &Operation) -> Option<String> {
    operation
        .request_body
        .as_ref()?
        .content
        .get("application/json")?
        .schema
        .as_ref()
        .and_then(|schema| match schema {
            RefOr::Ref(reference) => Some(reference.ref_location.clone()),
            RefOr::T(_) => None,
        })
}
