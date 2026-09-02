use std::collections::BTreeMap;

use thiserror::Error;
use utoipa::openapi::{OpenApi, RefOr, path::Operation};

use iaam_app::actions::OperationKey;

/// A route address resolved from the completed OpenAPI document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOperation {
    pub operation_id: String,
    pub method: String,
    pub path: String,
    pub request_schema: String,
}

/// The operation addresses advertised by computed actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionCatalog {
    operations: BTreeMap<&'static str, ActionOperation>,
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
    #[error("operation {operation_id} has no JSON request schema")]
    MissingRequestSchema { operation_id: String },
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

        let mut operations = BTreeMap::new();
        for key in [
            OperationKey::CreateAccount,
            OperationKey::CreateContour,
            OperationKey::RecordOwnerBalance,
            OperationKey::CreateCategoryRule,
        ] {
            let operation_id = key.as_str();
            let Some((path, method, operation)) = by_id.get(operation_id) else {
                return Err(ActionCatalogError::MissingActionOperation {
                    operation_id: operation_id.to_owned(),
                });
            };
            let request_schema = request_schema(operation).ok_or_else(|| {
                ActionCatalogError::MissingRequestSchema {
                    operation_id: operation_id.to_owned(),
                }
            })?;
            operations.insert(
                operation_id,
                ActionOperation {
                    operation_id: operation_id.to_owned(),
                    method: method.clone(),
                    path: path.clone(),
                    request_schema,
                },
            );
        }

        Ok(Self { operations })
    }

    /// Return the route address for an action operation.
    #[must_use]
    pub fn operation(&self, key: OperationKey) -> &ActionOperation {
        &self.operations[key.as_str()]
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
