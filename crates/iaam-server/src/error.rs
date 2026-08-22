//! Ответы об ошибках.
//!
//! Ошибка валидации — `422` с указанием поля, ожидаемого и полученного
//! значения (§13). Нарушение инварианта наружу уходит как `500`
//! с идентификатором корреляции и **без** числа: выдать результат
//! после доказанного нарушения тождества нельзя (§15.2).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use iaam_app::error::AppError;
use serde::Serialize;
use utoipa::ToSchema;

/// Тело ответа об ошибке.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    /// Машиночитаемый код. Агент разбирает его, а не текст.
    pub code: String,
    /// Пояснение для человека.
    pub message: String,
    /// Поле запроса, вызвавшее отказ.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// Идентификатор корреляции: по нему нарушение инварианта ищется
    /// в логах. Наружу не уходит ничего, кроме него.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ApiError {
    #[must_use]
    pub fn simple(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            field: None,
            expected: None,
            actual: None,
            correlation_id: None,
        }
    }
}

/// Ошибка обработчика.
///
/// Тело в `Box`: `Result<T, ApiFailure>` возвращают все обработчики,
/// и `clippy::result_large_err` справедливо возражает против варианта
/// ошибки размером в полтораста байт на каждом успешном пути.
#[derive(Debug)]
pub struct ApiFailure {
    pub status: StatusCode,
    pub body: Box<ApiError>,
}

impl ApiFailure {
    #[must_use]
    pub fn new(status: StatusCode, body: ApiError) -> Self {
        Self {
            status,
            body: Box::new(body),
        }
    }

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ApiError::simple("unauthorized", "требуется действующий токен"),
        )
    }

    #[must_use]
    pub fn forbidden(scope: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ApiError::simple(
                "forbidden",
                format!("права токена ({scope}) не позволяют эту операцию"),
            ),
        )
    }

    #[must_use]
    pub fn too_many_requests() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ApiError::simple("rate_limited", "слишком много запросов"),
        )
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(*self.body)).into_response()
    }
}

impl From<AppError> for ApiFailure {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Invalid {
                ref field,
                ref expected,
                ref actual,
            } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    field: Some(field.clone()),
                    expected: Some(expected.clone()),
                    actual: Some(actual.clone()),
                    correlation_id: None,
                },
            ),
            AppError::NotFound { what, ref id } => Self::new(
                StatusCode::NOT_FOUND,
                ApiError::simple("not_found", format!("не найдено: {what} {id}")),
            ),
            AppError::Invariant { correlation, .. } => {
                // Подробности остаются в логе: наружу уходит только код
                // и идентификатор корреляции.
                tracing::error!(%correlation, error = %error, "нарушен инвариант проекции");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError {
                        code: "invariant_violated".into(),
                        message: "результат не может быть выдан: нарушен внутренний инвариант"
                            .into(),
                        field: None,
                        expected: None,
                        actual: None,
                        correlation_id: Some(correlation.to_string()),
                    },
                )
            }
            AppError::Store(_) | AppError::Projection(_) => {
                tracing::error!(error = %error, "сценарий не выполнен");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::simple(error.code(), error.to_string()),
                )
            }
        }
    }
}
