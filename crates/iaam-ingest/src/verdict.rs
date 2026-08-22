//! Вердикты приёмки (§10.4).

use iaam_core::ids::EventId;
use serde::{Deserialize, Serialize};

/// Почему строка отклонена. Поле, ожидаемое и полученное — требование
/// §13 к ответам `422`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// Вердикт по одной строке.
///
/// Отдельного шага подтверждения в нормальном пути нет: есть отправка
/// и вердикт (§10.4). Вариант `Accepted` на этапе 1 недостижим —
/// подтверждать нечем, пока нет сверки (E2), и это записано в типе,
/// а не в комментарии к документации.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Записано, независимого подтверждения пока нет.
    Provisional { event: EventId },
    /// Уже записано ранее по ключу идемпотентности (§10.6).
    Duplicate { existing: EventId },
    /// Классификация неоднозначна: нужен ответ владельца.
    NeedsClassification { question: String },
    /// Операция вне периметра (§11): денежный эффект сохранён,
    /// экономика не достраивается.
    Unsupported { reason: String },
    /// Строка не разобрана.
    Rejected { rejection: Rejection },
}

impl Verdict {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Provisional { .. } => "provisional",
            Self::Duplicate { .. } => "duplicate",
            Self::NeedsClassification { .. } => "needs_classification",
            Self::Unsupported { .. } => "unsupported",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// Была ли строка записана в журнал.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        match self {
            Self::Provisional { .. } | Self::Duplicate { .. } => true,
            Self::NeedsClassification { .. } | Self::Unsupported { .. } | Self::Rejected { .. } => {
                false
            }
        }
    }
}
