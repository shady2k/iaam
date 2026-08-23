//! Вердикты приёмки (§10.4).

use iaam_core::ids::{AccountId, EventId};
use iaam_core::reconciliation::Dimension;
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
/// и вердикт (§10.4). Шесть вердиктов спеки — `Accepted`,
/// `Provisional`, `Discrepancy`, `NeedsReconciliation`,
/// `NeedsClassification`, `Unsupported`. `Duplicate` и `Rejected`
/// служебные: первый отвечает на повтор (§10.6), второй — на строку,
/// которую не удалось разобрать (§10.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Записано, сверка сошлась.
    Accepted { event: EventId },
    /// Записано, независимого подтверждения пока нет.
    Provisional { event: EventId },
    /// Записано, но сверка не сходится: владелец разбирается.
    Discrepancy {
        event: EventId,
        account: AccountId,
        dimension: Dimension,
        detail: String,
    },
    /// Сверять не с чем: требуется остаток от владельца.
    NeedsReconciliation {
        account: AccountId,
        dimension: Dimension,
    },
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
            Self::Accepted { .. } => "accepted",
            Self::Provisional { .. } => "provisional",
            Self::Discrepancy { .. } => "discrepancy",
            Self::NeedsReconciliation { .. } => "needs_reconciliation",
            Self::Duplicate { .. } => "duplicate",
            Self::NeedsClassification { .. } => "needs_classification",
            Self::Unsupported { .. } => "unsupported",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// Была ли строка записана в журнал.
    ///
    /// Расхождение записано: факт получен, и скрывать его до выяснения
    /// значило бы терять данные. Требование сверки — нет: там записывать
    /// нечего, вопрос задан владельцу.
    #[must_use]
    pub const fn is_recorded(&self) -> bool {
        match self {
            Self::Accepted { .. }
            | Self::Provisional { .. }
            | Self::Discrepancy { .. }
            | Self::Duplicate { .. } => true,
            Self::NeedsReconciliation { .. }
            | Self::NeedsClassification { .. }
            | Self::Unsupported { .. }
            | Self::Rejected { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_verdict() -> [Verdict; 8] {
        let event = EventId::new_random();
        let account = AccountId::new_random();
        [
            Verdict::Accepted { event },
            Verdict::Provisional { event },
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Cash,
                detail: "остаток на конец марта".to_owned(),
            },
            Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Cash,
            },
            Verdict::Duplicate { existing: event },
            Verdict::NeedsClassification {
                question: "перевод внутренний?".to_owned(),
            },
            Verdict::Unsupported {
                reason: "РЕПО".to_owned(),
            },
            Verdict::Rejected {
                rejection: Rejection {
                    field: "date".to_owned(),
                    expected: "ДД.ММ.ГГГГ".to_owned(),
                    actual: "вчера".to_owned(),
                },
            },
        ]
    }

    #[test]
    fn every_verdict_has_a_distinct_code_and_all_six_spec_verdicts_exist() {
        // §10.4 называет шесть вердиктов. Duplicate и Rejected служебные:
        // они отвечают на повтор и на неразобранную строку, а не на
        // результат приёмки. Проверяется и то, и другое — потерянный
        // вердикт превращается в молчание там, где владелец ждёт ответа.
        let all = every_verdict();
        let mut codes: Vec<&str> = all.iter().map(Verdict::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "коды вердиктов совпали");

        for verdict in [
            "accepted",
            "provisional",
            "discrepancy",
            "needs_reconciliation",
            "needs_classification",
            "unsupported",
        ] {
            assert!(codes.contains(&verdict), "вердикт {verdict} потерян");
        }
    }

    #[test]
    fn a_discrepancy_is_recorded_and_a_reconciliation_request_is_not() {
        // Расхождение — записанный факт с открытым вопросом. Требование
        // сверки — вопрос без факта. Слить их значит либо потерять
        // данные, либо записать в журнал то, чего не было.
        let event = EventId::new_random();
        let account = AccountId::new_random();
        assert!(
            Verdict::Discrepancy {
                event,
                account,
                dimension: Dimension::Positions,
                detail: String::new(),
            }
            .is_recorded()
        );
        assert!(
            !Verdict::NeedsReconciliation {
                account,
                dimension: Dimension::Positions,
            }
            .is_recorded()
        );
    }

    #[test]
    fn a_verdict_survives_a_serde_round_trip() {
        // Вердикт уходит наружу по REST: вариант, который не переживает
        // сериализацию, обнаружится у внешнего агента, а не здесь.
        for verdict in every_verdict() {
            let json = serde_json::to_string(&verdict).expect("сериализация");
            let back: Verdict = serde_json::from_str(&json).expect("разбор");
            assert_eq!(back, verdict);
        }
    }
}
