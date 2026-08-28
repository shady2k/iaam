//! Параметры выпуска: две оси времени и знание по каждому атрибуту (§2.4).
//!
//! Ось `observed_at` отвечает на вопрос «когда мы узнали», ось
//! `effective_from` — «с какой даты условия действуют». Одна ось на оба
//! вопроса заставляет отчёт воспроизвести условия, которых на выбранную
//! дату не существовало.

pub use iaam_core::bond::DefaultFlags;
use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;
use crate::schedule::Knowledge;

/// Снимок условий выпуска — набор утверждений **одного** источника
/// на **один** `observed_at`.
///
/// Собрать одну спецификацию из полей разных наблюдений нельзя: получится
/// выпуск, которого не существовало ни в один момент времени.
///
/// Текущего номинала здесь нет намеренно: он выводится из первоначального
/// и ряда возвратов. Хранить оба значило бы завести два источника истины.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueTerms {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    /// С какой даты условия действуют. MOEX её не сообщает.
    pub effective_from: Knowledge<Date>,
    pub maturity_date: Knowledge<Date>,
    pub initial_face_value: Knowledge<Dec>,
    /// Код валюты **как его назвал источник**. Перевод — словарём (§2.5).
    pub face_currency_code: Knowledge<String>,
    pub coupon_periods_per_year: Knowledge<u32>,
    /// База начисления дней. У MOEX всегда `Unknown` (§2.11).
    pub day_count: Knowledge<String>,
    /// Календарь. У MOEX всегда `Unknown` (§2.11).
    pub calendar: Knowledge<String>,
    pub default_flags: DefaultFlags,
}

impl IssueTerms {
    /// Действуют ли эти условия на дату `as_of`.
    ///
    /// При неизвестной `effective_from` снимок описывает условия на момент
    /// наблюдения и к более ранним датам не применяется: там действует
    /// предыдущий снимок либо `unknown`. Это отказ вместо угадывания.
    #[must_use]
    pub fn applies_at(&self, as_of: Date) -> bool {
        match &self.effective_from {
            Knowledge::Known(from) => as_of >= *from,
            Knowledge::Unknown => as_of >= self.observed_at.0.date(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::{date, datetime};

    fn minimal() -> IssueTerms {
        IssueTerms {
            instrument: InstrumentId::new_random(),
            observed_at: ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            effective_from: Knowledge::Unknown,
            maturity_date: Knowledge::Known(date!(2036 - 02 - 06)),
            initial_face_value: Knowledge::Known(Dec::new(Decimal::from(1000))),
            face_currency_code: Knowledge::Known("SUR".to_owned()),
            coupon_periods_per_year: Knowledge::Known(2),
            day_count: Knowledge::Unknown,
            calendar: Knowledge::Unknown,
            default_flags: DefaultFlags {
                declared: false,
                technical: false,
            },
        }
    }

    #[test]
    fn effective_from_is_a_separate_axis_from_observed_at() {
        // Правка эмитента, вступающая в силу с будущей даты, при одной оси
        // либо применяется ко всей истории, либо игнорируется на as_of.
        // Подставить observed_at вместо неизвестной даты вступления в силу
        // значит выдать догадку за факт.
        let terms = minimal();
        assert!(matches!(terms.effective_from, Knowledge::Unknown));
        assert!(terms.applies_at(date!(2026 - 08 - 27)));
        assert!(!terms.applies_at(date!(2026 - 08 - 26)));
    }

    #[test]
    fn day_count_and_calendar_have_no_default() {
        // MOEX не даёт ни того, ни другого — ни в графике, ни в описании
        // выпуска. Подставленный day-count даёт правдоподобно неверный НКД.
        let terms = minimal();
        assert!(terms.day_count.known().is_none());
        assert!(terms.calendar.known().is_none());
    }
}
