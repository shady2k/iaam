//! Доменные стратегии и их версии (§3.2).
//!
//! Стратегия — **не порт ввода-вывода**: она передаётся в ядро как
//! неизменяемый вход, поэтому чистота функционального ядра сохраняется.
//! Реестр закрытый: плагины в рантайме не нужны.

pub mod lot_disposal;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use lot_disposal::{FifoV1, LotDisposalRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LotRuleVersion(pub u32);

/// Реестр версионированных доменных правил.
///
/// На этапе 1 содержит только списание лотов. Налоговые правила
/// (`TaxRuleSet`, ключ `(TaxYear, TaxBaseKind)`) добавляются в эпике E5
/// по той же схеме.
pub struct RuleRegistry {
    lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>>,
}

impl RuleRegistry {
    /// Реестр с правилами по умолчанию.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>> = BTreeMap::new();
        lot_rules.insert(LotRuleVersion(1), Box::new(FifoV1));
        Self { lot_rules }
    }

    #[must_use]
    pub fn disposal_rule(&self, version: LotRuleVersion) -> Option<&dyn LotDisposalRule> {
        self.lot_rules.get(&version).map(|rule| rule.as_ref())
    }

    /// Наибольшая доступная версия. Используется, когда вызывающий
    /// не указал версию явно.
    #[must_use]
    pub fn latest_disposal_version(&self) -> Option<LotRuleVersion> {
        self.lot_rules.keys().next_back().copied()
    }
}

impl Default for RuleRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_fifo_v1() {
        let reg = RuleRegistry::with_defaults();
        let rule = reg
            .disposal_rule(LotRuleVersion(1))
            .expect("FIFO v1 зарегистрирован");
        assert_eq!(rule.id(), lot_disposal::RuleId::new(FifoV1::ID));
    }

    #[test]
    fn unknown_version_is_none_not_a_silent_fallback() {
        let reg = RuleRegistry::with_defaults();
        assert!(
            reg.disposal_rule(LotRuleVersion(99)).is_none(),
            "неизвестная версия не должна молча подменяться доступной"
        );
    }

    #[test]
    fn latest_version_is_reported() {
        let reg = RuleRegistry::with_defaults();
        assert_eq!(reg.latest_disposal_version(), Some(LotRuleVersion(1)));
    }

    #[test]
    fn the_default_registry_is_the_registry_with_defaults() {
        // `Default` — не отдельный пустой реестр: иначе вызывающий,
        // положившийся на него, молча остался бы без правил.
        let reg = RuleRegistry::default();
        assert_eq!(reg.latest_disposal_version(), Some(LotRuleVersion(1)));
    }

    #[test]
    fn the_registry_dispatches_disposal_through_the_rule_it_resolved() {
        // Проекция обязана ходить через реестр, а не звать FifoV1 напрямую:
        // здесь проверяется, что через `&dyn` вызов доходит до правила.
        use crate::dates::TradeDate;
        use crate::ids::InstrumentId;
        use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
        use crate::numeric::decimal::Dec;
        use lot_disposal::{DisposalInput, Lot, LotId};
        use rust_decimal::Decimal;
        use time::macros::date;

        let reg = RuleRegistry::with_defaults();
        let version = reg
            .latest_disposal_version()
            .expect("реестр не пуст по умолчанию");
        let rule = reg.disposal_rule(version).expect("версия разрешается");

        let lots = vec![Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            acquired: Some(TradeDate(date!(2026 - 01 - 10))),
            quantity: Quantity(Dec::new(Decimal::from(10))),
            cost_basis: Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
        }];
        let out = rule
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(4))),
            })
            .expect("списание выполнимо");

        assert_eq!(out.rule, lot_disposal::RuleId::new(FifoV1::ID));
        assert_eq!(
            out.basis_released,
            Money::new(PostedMinor::new(40_000), CurrencyCode::Rub)
        );
    }
}
