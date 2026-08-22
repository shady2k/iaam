//! Ошибки сценариев.
//!
//! Разделение по §15.2: неполнота данных ошибкой не является и уходит
//! в отчёт блоком качества; нарушение инварианта отменяет отчёт и уходит
//! в лог с идентификатором корреляции.

use iaam_core::projection::ProjectionError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("хранилище недоступно: {0}")]
    Store(String),
    #[error("не найдено: {what} {id}")]
    NotFound { what: &'static str, id: String },
    #[error("запрос некорректен: поле {field}, ожидалось {expected}, получено {actual}")]
    Invalid {
        field: String,
        expected: String,
        actual: String,
    },
    #[error("нарушен внутренний инвариант, идентификатор корреляции {correlation}")]
    Invariant {
        correlation: Uuid,
        #[source]
        source: ProjectionError,
    },
    #[error("проекция не построена: {0}")]
    Projection(#[source] ProjectionError),
}

impl AppError {
    /// Проекция превращается в ошибку приложения так, чтобы нарушение
    /// инварианта нельзя было спутать с обычным отказом: у первого
    /// появляется идентификатор корреляции для логов (§15.2).
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
            Self::NotFound { .. } => "not_found",
            Self::Invalid { .. } => "invalid_request",
            Self::Invariant { .. } => "invariant_violated",
            Self::Projection(_) => "projection_failed",
        }
    }
}
