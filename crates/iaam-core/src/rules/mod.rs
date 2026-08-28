//! Доменные стратегии и их версии (§3.2).
//!
//! Стратегия — **не порт ввода-вывода**: она передаётся в ядро как
//! неизменяемый вход, поэтому чистота функционального ядра сохраняется.
//! Реестр закрытый: плагины в рантайме не нужны.

mod accrued_interest;
pub mod cashflow;
pub mod amortisation;
pub mod lot_disposal;
pub mod quotation;
pub mod valuation;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use accrued_interest::{
    AccruedInterestError, AccruedInterestRule, AccruedInterestRuleVersion, AccruedInterestV1,
};
use amortisation::{AmortisationRule, AmortisationRuleVersion, ProRataV1};
use lot_disposal::{FifoV1, LotDisposalRule};

pub use quotation::{QuotationError, QuotationRule, QuotationRuleVersion, QuotationV1};
pub use cashflow::{
    CashflowError, CashflowInput, CashflowPlan, CashflowProjection, CashflowProjectionV1,
    CashflowProjectionVersion, ExpectedPosting, PostingKind,
};
pub use valuation::{
    PriceSelectionResult, SourcePriorityVersion, ValuationPolicyV1, ValuationPolicyVersion,
    ValuationRule,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LotRuleVersion(pub u32);

/// Реестр версионированных доменных правил.
///
/// Реестр хранит независимые наборы правил: списание лотов и выбор цены.
pub struct RuleRegistry {
    lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>>,
    valuation_rules: BTreeMap<ValuationPolicyVersion, Box<dyn ValuationRule>>,
    /// Отдельная карта, а не расширение `lot_rules`: списание лотов —
    /// выбор владельца, амортизация — событие выпуска, и общая версия
    /// связала бы два независимых решения.
    amortisation_rules: BTreeMap<AmortisationRuleVersion, Box<dyn AmortisationRule>>,
    quotation_rules: BTreeMap<QuotationRuleVersion, Box<dyn QuotationRule>>,
    cashflow_rules: BTreeMap<CashflowProjectionVersion, Box<dyn CashflowProjection>>,
    accrued_interest_rules: BTreeMap<AccruedInterestRuleVersion, Box<dyn AccruedInterestRule>>,
}

impl RuleRegistry {
    /// Реестр с правилами по умолчанию.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut lot_rules: BTreeMap<LotRuleVersion, Box<dyn LotDisposalRule>> = BTreeMap::new();
        lot_rules.insert(LotRuleVersion(1), Box::new(FifoV1));
        let mut valuation_rules: BTreeMap<ValuationPolicyVersion, Box<dyn ValuationRule>> =
            BTreeMap::new();
        valuation_rules.insert(
            ValuationPolicyVersion(1),
            Box::new(ValuationPolicyV1::default()),
        );
        let mut amortisation_rules: BTreeMap<AmortisationRuleVersion, Box<dyn AmortisationRule>> =
            BTreeMap::new();
        amortisation_rules.insert(AmortisationRuleVersion(1), Box::new(ProRataV1));
        let mut quotation_rules: BTreeMap<QuotationRuleVersion, Box<dyn QuotationRule>> =
            BTreeMap::new();
        quotation_rules.insert(QuotationRuleVersion(1), Box::new(QuotationV1));
        let mut accrued_interest_rules: BTreeMap<
            AccruedInterestRuleVersion,
            Box<dyn AccruedInterestRule>,
        > = BTreeMap::new();
        let mut cashflow_rules: BTreeMap<
            CashflowProjectionVersion,
            Box<dyn CashflowProjection>,
        > = BTreeMap::new();
        cashflow_rules.insert(CashflowProjectionVersion(1), Box::new(CashflowProjectionV1));
        accrued_interest_rules.insert(AccruedInterestRuleVersion(1), Box::new(AccruedInterestV1));

        Self {
            lot_rules,
            valuation_rules,
            amortisation_rules,
            quotation_rules,
            accrued_interest_rules,
            cashflow_rules,
        }
    }

    #[must_use]
    pub fn amortisation_rule(
        &self,
        version: AmortisationRuleVersion,
    ) -> Option<&dyn AmortisationRule> {
        self.amortisation_rules
            .get(&version)
            .map(|rule| rule.as_ref())
    }

    #[must_use]
    pub fn accrued_interest_rule(
        &self,
        version: AccruedInterestRuleVersion,
    ) -> Option<&dyn AccruedInterestRule> {
        self.accrued_interest_rules
            .get(&version)
            .map(|rule| rule.as_ref())
    }

    #[must_use]
    pub fn cashflow_rule(
        &self,
        version: CashflowProjectionVersion,
    ) -> Option<&dyn CashflowProjection> {
        self.cashflow_rules.get(&version).map(|rule| rule.as_ref())
    }

    /// Наибольшая доступная версия правила построения потока.
    #[must_use]
    pub fn latest_cashflow_version(&self) -> Option<CashflowProjectionVersion> {
        self.cashflow_rules.keys().next_back().copied()
    }

    /// Наибольшая доступная версия правила амортизации.
    #[must_use]
    pub fn latest_amortisation_version(&self) -> Option<AmortisationRuleVersion> {
        self.amortisation_rules.keys().next_back().copied()
    }

    #[must_use]
    pub fn valuation_rule(&self, version: ValuationPolicyVersion) -> Option<&dyn ValuationRule> {
        self.valuation_rules.get(&version).map(|rule| rule.as_ref())
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

    #[must_use]
    pub fn quotation_rule(&self, version: QuotationRuleVersion) -> Option<&dyn QuotationRule> {
        self.quotation_rules.get(&version).map(|rule| rule.as_ref())
    }

    /// Наибольшая доступная версия правила пересчёта котировки.
    #[must_use]
    pub fn latest_quotation_version(&self) -> Option<QuotationRuleVersion> {
        self.quotation_rules.keys().next_back().copied()
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
        use lot_disposal::{DisposalInput, Lot, LotId, PrincipalState};
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
            accrued_interest_paid: None,
            received_to_date: None,
            principal: PrincipalState::Unknown,
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
    #[test]
    fn registry_resolves_valuation_v1() {
        let reg = RuleRegistry::with_defaults();
        let rule = reg
            .valuation_rule(ValuationPolicyVersion(1))
            .expect("политика оценки v1 зарегистрирована");
        assert_eq!(rule.version(), ValuationPolicyVersion(1));
    }

    #[test]
    fn unknown_valuation_policy_version_is_none_not_a_silent_default() {
        let reg = RuleRegistry::with_defaults();
        assert!(reg.valuation_rule(ValuationPolicyVersion(2)).is_none());
    }
    #[test]
    fn registry_resolves_cashflow_v1() {
        let reg = RuleRegistry::with_defaults();
        assert!(reg.cashflow_rule(CashflowProjectionVersion(1)).is_some());
    }

    #[test]
    fn latest_cashflow_version_is_reported() {
        let reg = RuleRegistry::with_defaults();
        assert_eq!(
            reg.latest_cashflow_version(),
            Some(CashflowProjectionVersion(1))
        );
    }
}
