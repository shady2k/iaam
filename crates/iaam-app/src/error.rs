//! Scenario errors.
//!
//! Distinction under §15.2: incomplete data is not an error and goes
//! into the report as a quality block; an invariant violation aborts the report and goes
//! into the log with a correlation identifier.

use iaam_core::event::correction::CorrectionError;
use iaam_core::perimeter::PerimeterError;
use iaam_core::projection::ProjectionError;
use iaam_core::projection::active_instruments::ActiveInstrumentsError;
use iaam_core::projection::money_flow::MoneyFlowError;
use iaam_core::reconciliation::observed::ObserveError;
use iaam_core::report::assets::AssetSnapshotError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("store unavailable: {0}")]
    Store(String),
    #[error("category group {id} is retired")]
    CategoryGroupRetired { id: String },
    #[error("not found: {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("request is invalid: field {field}, expected {expected}, received {actual}")]
    Invalid {
        field: String,
        expected: String,
        actual: String,
    },
    #[error("internal invariant violated, correlation identifier {correlation}")]
    Invariant {
        correlation: Uuid,
        #[source]
        source: ProjectionError,
    },
    /// Reference data invariant violated: a code resolves to more than one
    /// instrument, meaning that the trigger has fired
    /// `instrument_aliases_do_not_overlap`.
    /// Separate from `Invariant`, because that carries a source from the projection domain
    /// projection: putting any variant there merely to satisfy the signature would
    /// send an investigator to inspect snapshots instead of the reference data schema.
    #[error("reference data invariant violated, correlation identifier {correlation}: {detail}")]
    DirectoryInvariant { correlation: Uuid, detail: String },
    #[error("projection not built: {0}")]
    Projection(#[source] ProjectionError),
    #[error("money flow not built: {0}")]
    MoneyFlow(#[from] MoneyFlowError),
    /// A journal slice is unsuitable for reconciliation: an undated event,
    /// a balance overflow. Separate from `Projection`, because
    /// these are different causes for an external agent: one means an invalid
    /// slice, the other — an inability to verify the data.
    #[error("reconciliation not built: {0}")]
    Reconciliation(#[source] ObserveError),
    #[error("perimeter not assessed: {0}")]
    Perimeter(#[source] PerimeterError),
    /// The journal's corrections do not resolve: a reversal or replacement points at
    /// an event the slice does not contain, or one event is corrected twice. Separate
    /// from `Reconciliation`, because that names an inability to verify data, while
    /// this names a journal whose own links are inconsistent — and reporting it as
    /// «reconciliation not built» would send an investigator to the reconciliation
    /// register instead of the corrections in the journal.
    #[error("journal corrections do not resolve: {0}")]
    Correction(#[source] CorrectionError),
    /// The capability is not enabled by configuration, not broken: encryption
    /// of access to the broker without a key. Separate from `Store`, because
    /// these are different causes for an external agent: one is fixed by configuring
    /// the server, the other — by retrying the request.
    #[error("{what} is not configured")]
    NotConfigured { what: &'static str },
    /// The system source of randomness has failed. Separate from `Store`,
    /// because this is not a store failure and is not fixed by retrying the request,
    /// but by correcting the machine state; it is also separate because a secret,
    /// obtained by unknown means, must never be issued under any circumstances (§14).
    #[error("randomness source unavailable: {0}")]
    Random(String),
    /// The record already exists, and a second identical one would mean that it is unclear,
    /// which one is being used. Separate from `Store`, because it is fixed
    /// not by retrying the request, but by revoking the active record.
    #[error("{what}")]
    Conflict { what: String },
    /// There is no way to build the synchronisation schedule: deriving active securities
    /// from the journal failed numerically or because the journal's corrections were invalid.
    /// Separate from `Reconciliation`, because reconciliation is irrelevant here, while
    /// “reconciliation not built” would send an investigator to inspect the register instead
    /// of the quantity journal.
    #[error("synchronisation schedule not built: {0}")]
    Schedule(#[source] ActiveInstrumentsError),
    /// The asset snapshot did not fold: a currency mixed inside one total, or a
    /// decimal overflow while adding rows. Separate from `Projection`, because
    /// the projection succeeded — it is the summation over its rows that did
    /// not, and reporting it as a projection failure would send an investigator
    /// to the journal rather than to the totals.
    #[error("asset snapshot not built: {0}")]
    AssetSnapshot(#[source] AssetSnapshotError),
}

impl From<ObserveError> for AppError {
    fn from(error: ObserveError) -> Self {
        Self::Reconciliation(error)
    }
}

impl From<PerimeterError> for AppError {
    fn from(error: PerimeterError) -> Self {
        Self::Perimeter(error)
    }
}

impl AppError {
    /// A projection is converted into an application error so that an invariant
    /// violation cannot be confused with an ordinary failure: the former
    /// gets a correlation identifier for the logs (§15.2).
    #[must_use]
    pub fn from_projection(error: ProjectionError) -> Self {
        if error.is_invariant_violation() {
            Self::Invariant {
                correlation: Uuid::new_v4(),
                source: error,
            }
        } else {
            Self::Projection(error)
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Store(_) => "store_unavailable",
            Self::CategoryGroupRetired { .. } => "category_group_retired",
            Self::NotFound { .. } => "not_found",
            Self::Invalid { .. } => "invalid_request",
            Self::Invariant { .. } => "invariant_violated",
            Self::DirectoryInvariant { .. } => "directory_invariant_violated",
            Self::Projection(_) => "projection_failed",
            Self::MoneyFlow(_) => "money_flow_failed",
            Self::Reconciliation(_) => "reconciliation_failed",
            Self::Schedule(_) => "schedule_not_built",
            Self::AssetSnapshot(_) => "asset_snapshot_not_built",
            Self::Perimeter(_) => "perimeter_assessment_failed",
            Self::Correction(_) => "corrections_do_not_resolve",
            Self::NotConfigured { .. } => "not_configured",
            Self::Random(_) => "random_unavailable",
            Self::Conflict { .. } => "already_exists",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_error_has_a_machine_readable_code() {
        // The code goes in the response body: the external agent uses it to decide,
        // whether to retry the request. An empty string is indistinguishable from «no code»,
        // and a single code for all errors — from «something went wrong».
        assert_eq!(
            AppError::Store("no connection".into()).code(),
            "store_unavailable"
        );
        assert_eq!(
            AppError::NotFound {
                what: "environment",
                id: "does not exist".into(),
            }
            .code(),
            "not_found"
        );
        assert_eq!(
            AppError::Invalid {
                field: "as_of".into(),
                expected: "date in YYYY-MM-DD format".into(),
                actual: "yesterday".into(),
            }
            .code(),
            "invalid_request"
        );
        assert_eq!(
            AppError::Invariant {
                correlation: Uuid::new_v4(),
                source: ProjectionError::SnapshotFingerprintMismatch,
            }
            .code(),
            "invariant_violated"
        );
        assert_eq!(
            AppError::DirectoryInvariant {
                correlation: Uuid::new_v4(),
                detail: "code ticker:ABC on 2026-08-25 resolves to 2 instruments".into(),
            }
            .code(),
            "directory_invariant_violated"
        );
        assert_eq!(
            AppError::Projection(ProjectionError::SnapshotFingerprintMismatch).code(),
            "projection_failed"
        );
        assert_eq!(
            AppError::NotConfigured {
                what: "broker access encryption",
            }
            .code(),
            "not_configured"
        );
        assert_eq!(
            AppError::Random("source closed".into()).code(),
            "random_unavailable"
        );
        assert_eq!(
            AppError::Conflict {
                what: "access already configured".into(),
            }
            .code(),
            "already_exists"
        );
    }
}
