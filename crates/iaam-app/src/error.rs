//! Ошибки сценариев.
//!
//! Разделение по §15.2: неполнота данных ошибкой не является и уходит
//! в отчёт блоком качества; нарушение инварианта отменяет отчёт и уходит
//! в лог с идентификатором корреляции.

use iaam_core::perimeter::PerimeterError;
use iaam_core::projection::ProjectionError;
use iaam_core::reconciliation::observed::ObserveError;
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
    /// Инвариант справочника нарушен: код разрешается более чем в один
    /// инструмент, то есть пробит триггер
    /// `instrument_aliases_do_not_overlap`.
    /// Отдельно от `Invariant`, потому что тот несёт источник из домена
    /// проекции: подставить туда любой вариант ради сигнатуры значит
    /// отправить разбирающегося смотреть снимки вместо схемы справочника.
    #[error("нарушен инвариант справочника, идентификатор корреляции {correlation}: {detail}")]
    DirectoryInvariant { correlation: Uuid, detail: String },
    #[error("проекция не построена: {0}")]
    Projection(#[source] ProjectionError),
    /// Срез журнала не годится для сверки: событие без даты,
    /// переполнение остатка. Отдельно от `Projection`, потому что
    /// внешнему агенту это разные поводы: одно означает неверный срез,
    /// другое — невозможность подтвердить данные.
    #[error("сверка не построена: {0}")]
    Reconciliation(#[source] ObserveError),
    #[error("периметр не оценён: {0}")]
    Perimeter(#[source] PerimeterError),
    /// Возможность не включена настройкой, а не сломана: шифрование
    /// доступа к брокеру без ключа. Отдельно от `Store`, потому что
    /// внешнему агенту это разные поводы: одно чинится настройкой
    /// сервера, другое — повтором запроса.
    #[error("{what} не настроено")]
    NotConfigured { what: &'static str },
    /// Системный источник случайности отказал. Отдельно от `Store`,
    /// потому что это не сбой хранилища и чинится не повтором запроса,
    /// а состоянием машины; и отдельно потому, что выдать секрет,
    /// полученный неизвестно чем, нельзя ни при каких условиях (§14).
    #[error("источник случайности недоступен: {0}")]
    Random(String),
    /// Запись уже есть, и вторая такая же означала бы, что неизвестно,
    /// какой из них пользуются. Отдельно от `Store`, потому что чинится
    /// не повтором запроса, а отзывом действующей записи.
    #[error("{what}")]
    Conflict { what: String },
    /// Расписание синхронизации построить нечем: вывод активных бумаг
    /// из журнала переполнился. Отдельно от `Reconciliation`, потому
    /// что сверка здесь ни при чём, а «сверка не построена» отправило бы
    /// разбирающегося читать реестр сверки вместо журнала количеств.
    #[error("расписание синхронизации не построено: {0}")]
    Schedule(#[source] iaam_core::numeric::NumericError),
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
            Self::DirectoryInvariant { .. } => "directory_invariant_violated",
            Self::Projection(_) => "projection_failed",
            Self::Reconciliation(_) => "reconciliation_failed",
            Self::Schedule(_) => "schedule_not_built",
            Self::Perimeter(_) => "perimeter_assessment_failed",
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
        // Код уходит в тело ответа: внешний агент решает по нему,
        // повторять ли запрос. Пустая строка неотличима от «кода нет»,
        // а один код на все ошибки — от «что-то пошло не так».
        assert_eq!(
            AppError::Store("нет соединения".into()).code(),
            "store_unavailable"
        );
        assert_eq!(
            AppError::NotFound {
                what: "контур",
                id: "нет такого".into(),
            }
            .code(),
            "not_found"
        );
        assert_eq!(
            AppError::Invalid {
                field: "as_of".into(),
                expected: "дата вида ГГГГ-ММ-ДД".into(),
                actual: "вчера".into(),
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
                detail: "код ticker:ABC на 2026-08-25 разрешается в 2 инструмента".into(),
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
                what: "шифрование доступа к брокеру",
            }
            .code(),
            "not_configured"
        );
        assert_eq!(
            AppError::Random("источник закрыт".into()).code(),
            "random_unavailable"
        );
        assert_eq!(
            AppError::Conflict {
                what: "доступ уже заведён".into(),
            }
            .code(),
            "already_exists"
        );
    }
}
