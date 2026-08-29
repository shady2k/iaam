//! Знание о расчётах по событиям, меняющим количество.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

use crate::dates::EventDates;
use crate::event::provenance::ParserVersion;

/// Что известно о дате фактического расчёта по событию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementKnowledge {
    /// Источник сообщил дату расчётов.
    Exact(Date),
    /// Известна дата сделки; расчёт произошёл где-то внутри полосы.
    Bounded { earliest: Date, latest: Date },
    /// Смысл даты источника не доказан: расчёт мог произойти когда угодно.
    Unbounded,
}

/// Применилось ли событие к дате.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Yes,
    No,
    Maybe,
}

impl SettlementKnowledge {
    /// Интервал замкнут с обоих концов: внутридневного времени нет, поэтому
    /// расчёт ровно в `latest` возможен. `Exact(d)` — тот же вырожденный
    /// интервал `[d, d]`, поэтому на самой `d` ответ `Maybe`.
    #[must_use]
    pub fn applied_before(&self, day: Date) -> Applied {
        match self {
            Self::Exact(date) => Self::bounded(*date, *date, day),
            Self::Bounded { earliest, latest } => Self::bounded(*earliest, *latest, day),
            Self::Unbounded => Applied::Maybe,
        }
    }

    fn bounded(earliest: Date, latest: Date, day: Date) -> Applied {
        if latest < day {
            Applied::Yes
        } else if day < earliest {
            Applied::No
        } else {
            Applied::Maybe
        }
    }
}

/// Версия таблицы полос расчётов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SettlementLagPolicyVersion(pub u32);

/// Максимальная задержка расчётов по профилю источника.
///
/// Дни календарные, а не рабочие: производственного календаря в ядре нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementLagPolicy {
    version: SettlementLagPolicyVersion,
    /// Таблица v1 намеренно пуста: ни один профиль пока не имеет письменного
    /// обоснования верхней границы задержки. Добавление профиля требует такого
    /// обоснования, а не наблюдения обычаев вроде «обычно T+1».
    max_calendar_days: BTreeMap<ParserVersion, u32>,
}

impl SettlementLagPolicy {
    pub const VERSION: SettlementLagPolicyVersion = SettlementLagPolicyVersion(1);

    #[must_use]
    pub fn new(version: SettlementLagPolicyVersion) -> Self {
        Self {
            version,
            max_calendar_days: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn v1() -> Self {
        Self::new(Self::VERSION)
    }

    #[must_use]
    pub const fn version(&self) -> SettlementLagPolicyVersion {
        self.version
    }

    /// Добавить доказанную календарную полосу для конкретной версии парсера.
    #[must_use]
    pub fn with_profile(mut self, profile: ParserVersion, max_calendar_days: u32) -> Self {
        self.max_calendar_days.insert(profile, max_calendar_days);
        self
    }

    /// Вывести знание о расчёте из дат события и профиля парсера.
    #[must_use]
    pub fn knowledge(
        &self,
        dates: &EventDates,
        parser_version: &ParserVersion,
    ) -> SettlementKnowledge {
        if let Some(settled) = dates.settled {
            return SettlementKnowledge::Exact(settled.0);
        }

        let Some(trade) = dates.trade else {
            return SettlementKnowledge::Unbounded;
        };
        let Some(max_days) = self.max_calendar_days.get(parser_version) else {
            return SettlementKnowledge::Unbounded;
        };
        let Some(latest) = trade.0.checked_add(Duration::days(i64::from(*max_days))) else {
            return SettlementKnowledge::Unbounded;
        };
        SettlementKnowledge::Bounded {
            earliest: trade.0,
            latest,
        }
    }
}

impl Default for SettlementLagPolicy {
    fn default() -> Self {
        Self::v1()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::{SettledDate, TradeDate};

    #[test]
    fn exact_settlement_uses_a_closed_calendar_boundary() {
        // На самой дате расчёта нельзя приписать событие началу дня без
        // времени: это сохранило бы меньше неопределённости, чем есть.
        let date = time::macros::date!(2026 - 03 - 10);
        let knowledge = SettlementKnowledge::Exact(date);
        assert_eq!(
            knowledge.applied_before(date.previous_day().unwrap()),
            Applied::No
        );
        assert_eq!(knowledge.applied_before(date), Applied::Maybe);
        assert_eq!(
            knowledge.applied_before(date.next_day().unwrap()),
            Applied::Yes
        );
    }

    #[test]
    fn policy_prefers_exact_settlement_and_keeps_unknown_profiles_unbounded() {
        // Одинаковое поле даты у разных профилей не даёт права переносить
        // доказанную полосу одного источника на другой.
        let trade = time::macros::date!(2026 - 03 - 10);
        let exact = EventDates::for_trade(
            TradeDate(trade),
            Some(SettledDate(time::macros::date!(2026 - 03 - 11))),
        );
        let profile = ParserVersion("broker/1".to_owned());
        let policy = SettlementLagPolicy::default().with_profile(profile.clone(), 2);
        assert_eq!(
            policy.knowledge(&exact, &profile),
            SettlementKnowledge::Exact(time::macros::date!(2026 - 03 - 11))
        );

        let without_settled = EventDates::for_trade(TradeDate(trade), None);
        assert_eq!(
            policy.knowledge(&without_settled, &ParserVersion("other/1".to_owned())),
            SettlementKnowledge::Unbounded
        );
        assert_eq!(
            policy.knowledge(&without_settled, &profile),
            SettlementKnowledge::Bounded {
                earliest: trade,
                latest: time::macros::date!(2026 - 03 - 12),
            }
        );
    }
}
