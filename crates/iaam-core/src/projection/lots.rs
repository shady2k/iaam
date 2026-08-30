//! Книга лотов (§4.12).
//!
//! Лоты строятся **по типу события**: покупка добавляет партию, продажа
//! списывает её версионированным правилом из реестра. Количество бумаг
//! при этом считается независимо — по ногам события (`super::balances`).
//!
//! Восстановленная позиция без документированной стоимости (§10.7)
//! **не превращается в лот с нулевой стоимостью**: она хранится отдельным
//! количеством, списывается первой и делает реализованный результат
//! невычислимым. Нулевая заглушка здесь означала бы выдуманную прибыль,
//! равную всей выручке.

use super::ownership::Ownership;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dates::TradeDate;
use crate::event::Event;
use crate::event::allocation::BasisAllocation;
use crate::event::corporate_action::{BasisTransferRule, CorporateAction};
use crate::event::kind::{DateCertainty, EventKind, IncomeKind, TradeSide};
use crate::event::offer::OfferExerciseAction;
use crate::ids::{AccountId, EventId, InstrumentId};
use crate::money::{Money, MoneyError, Quantity};
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;
use crate::rules::amortisation::{AmortisationError, AmortisationRuleVersion};
use crate::rules::lot_disposal::{
    DisposalError, DisposalInput, DisposalResult, Lot, LotId, PrincipalError, PrincipalState,
    RuleId, split_basis,
};
use crate::rules::{LotRuleVersion, RuleRegistry};
use crate::settlement::{SettlementKnowledge, SettlementLagPolicy};

/// Лоты не различают место хранения: перевод бумаги между депозитариями
/// не является приобретением и не создаёт новой партии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LotKey {
    pub account: AccountId,
    pub instrument: InstrumentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LotError {
    #[error("в реестре нет правила списания версии {version:?}")]
    UnknownRule { version: LotRuleVersion },
    #[error("продажа {event:?} без предшествующей позиции по инструменту {instrument:?}")]
    SaleWithoutPosition {
        event: EventId,
        instrument: InstrumentId,
    },
    #[error("в реестре нет правила амортизации версии {version:?}")]
    UnknownAmortisationRule { version: AmortisationRuleVersion },
    #[error(
        "событие названо на количество {declared:?}, а на счёте по этой бумаге {held:?}: \
         корпоративное действие касается всей позиции, и расхождение — брак источника"
    )]
    QuantityMismatch { held: Quantity, declared: Quantity },
    #[error(transparent)]
    Amortisation(#[from] AmortisationError),
    #[error(transparent)]
    Principal(#[from] PrincipalError),
    #[error(transparent)]
    Disposal(#[from] DisposalError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error(transparent)]
    Numeric(#[from] NumericError),
}

/// Почему реализованный результат по инструменту не вычисляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisGap {
    /// Позиция восстановлена без документированной стоимости (§10.7).
    RestoredWithoutBasis,
    /// Доля разнесения неизвестна, поэтому долю возвращённой при
    /// амортизации стоимости считать не от чего (§4.9). Факт применён,
    /// реализованный результат — невычислим.
    AmortisationAllocationUnknown,
}

impl BasisGap {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RestoredWithoutBasis => "restored_without_basis",
            Self::AmortisationAllocationUnknown => "amortisation_allocation_unknown",
        }
    }
}

/// Группа лотов, приобретённых в одну дату.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cohort {
    pub acquired: TradeDate,
    pub quantity: Quantity,
    pub cost_basis: Money,
    #[serde(default)]
    pub acquisition_basis: Option<Money>,
    pub accrued_interest_paid: Option<Money>,
    pub received_to_date: Option<Money>,
}

/// Почему пожизненная метрика по когортам не вычисляется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum CohortGap {
    #[error("дата приобретения неизвестна")]
    AcquisitionDateUnknown,
    #[error("вид дохода неизвестен")]
    IncomeKindUnknown,
    #[error("позиция восстановлена без документированной стоимости")]
    RestoredWithoutBasis,
    #[error("количество лотов переполняется при сложении")]
    InconsistentQuantity,
    #[error("валюты стоимостей лотов различаются")]
    InconsistentCostBasisCurrency,
    #[error("стоимость лотов переполняется при сложении")]
    InconsistentCostBasisOverflow,
    #[error("валюты дополнительных денежных величин лотов различаются")]
    InconsistentOptionalMoneyCurrency,
    #[error("дополнительные денежные величины лотов переполняются при сложении")]
    InconsistentOptionalMoneyOverflow,
}

/// Приобретения, когда-либо наблюдённые по паре (счёт, инструмент).
///
/// Отдельно от живых партий, потому что выбытие партии не отменяет
/// того, что бумага в тот день уже была: граница владения, посчитанная
/// по оставшимся партиям, после продажи ранней партии поднимается и
/// прячет пропуск выплаты за период, когда бумага была на руках.
/// Величина монотонна: выбытие её не двигает.
///
/// `#[serde(default)]` намеренно нет: снимок без неё выглядел бы как
/// позиция без истории приобретений, то есть выдавал бы «не владел»
/// за «не знаем». Снимки прежней версии отвергает `PROJECTION_VERSION`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AcquisitionHistory {
    /// Самая ранняя наблюдённая дата приобретения.
    earliest: Option<TradeDate>,
    /// Наблюдалось приобретение без даты: партия без даты сделки либо
    /// количество, восстановленное без стоимости. Признак липкий по той
    /// же причине, по которой липка сама дата: выбытие безымянной
    /// партии не превращает неизвестную границу в известную.
    undated: bool,
}

impl AcquisitionHistory {
    fn observe(&mut self, acquired: Option<TradeDate>) {
        match acquired {
            Some(date) => {
                self.earliest = Some(match self.earliest {
                    Some(known) => known.min(date),
                    None => date,
                });
            }
            None => self.undated = true,
        }
    }

    /// Нижняя граница владения. `None`, когда наблюдалось приобретение
    /// без даты: любая граница по остальным партиям была бы позже
    /// настоящей и скрыла бы пропуск.
    const fn lower_bound(self) -> Option<TradeDate> {
        if self.undated { None } else { self.earliest }
    }
}

/// Лоты одного инструмента на одном счёте.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentLots {
    /// Количество, восстановленное без стоимости. Списывается первым:
    /// оно приобретено раньше всего, что система видела.
    unpriced: Quantity,
    /// Партии в порядке приобретения.
    lots: Vec<Lot>,
    /// Реализованный результат до налога. `None`, если хотя бы одно
    /// выбытие затронуло количество без стоимости.
    realized: Option<Money>,
    /// Суммарная стоимость всех приобретений с документированной ценой.
    acquired_basis: Option<Money>,
    /// Суммарная стоимость, списанная при выбытиях.
    released_basis: Option<Money>,
    gap: Option<BasisGap>,
    /// Факт выплаты неизвестного вида, который нельзя приписать лотам.
    #[serde(default)]
    income_kind_unknown: bool,
    /// Известная выплата, часть которой пришлась на восстановленное
    /// количество либо на позицию без лотов.
    #[serde(default)]
    unallocated_income: Option<Money>,
    /// Доля известной выплаты, пришедшаяся на восстановленное количество.
    #[serde(default)]
    unpriced_income: Option<Money>,
    /// Приобретения, когда-либо наблюдённые по паре.
    acquisitions: AcquisitionHistory,
    /// История изменений количества со знанием о расчёте.
    ///
    /// `#[serde(default)]` намеренно нет: снимок без истории нельзя честно
    /// считать позицией без владения; его должен отвергнуть номер проекции.
    ownership: super::ownership::OwnershipHistory,
}

/// Пустая книга по инструменту. Пишется вручную, потому что `Quantity`
/// намеренно не реализует `Default`: нулевое количество должно возникать
/// осознанно, а не как значение по умолчанию неизвестного поля (§4.9).
impl Default for InstrumentLots {
    fn default() -> Self {
        Self {
            unpriced: Quantity::zero(),
            lots: Vec::new(),
            realized: None,
            acquired_basis: None,
            released_basis: None,
            gap: None,
            income_kind_unknown: false,
            unallocated_income: None,
            unpriced_income: None,
            acquisitions: AcquisitionHistory::default(),
            ownership: super::ownership::OwnershipHistory::default(),
        }
    }
}

impl InstrumentLots {
    #[must_use]
    pub fn lots(&self) -> &[Lot] {
        &self.lots
    }

    #[must_use]
    pub const fn unpriced(&self) -> Quantity {
        self.unpriced
    }

    #[must_use]
    pub const fn realized(&self) -> Option<Money> {
        self.realized
    }

    #[must_use]
    pub const fn gap(&self) -> Option<BasisGap> {
        self.gap
    }

    #[must_use]
    pub const fn income_kind_unknown(&self) -> bool {
        self.income_kind_unknown
    }

    /// Самая ранняя дата приобретения, когда-либо наблюдённая по паре.
    ///
    /// Считается по всей истории, а не по живым партиям: продажа ранней
    /// партии границу владения не поднимает, иначе пропуск выплаты за
    /// период, когда бумага была на руках, остался бы неназванным.
    ///
    /// `None`, если есть количество, восстановленное без стоимости,
    /// либо хоть у одного приобретения даты не было: границу владения
    /// тогда провести нечем, а провести её приблизительно значило бы
    /// либо выдумать дефект, либо скрыть настоящий.
    #[must_use]
    pub fn earliest_acquired(&self) -> Option<TradeDate> {
        if !self.unpriced.0.is_zero() {
            return None;
        }
        self.acquisitions.lower_bound()
    }

    /// Статус владения на дату с учётом всех изменений количества.
    #[must_use]
    pub fn ownership_at(&self, day: time::Date) -> Ownership {
        self.ownership.ownership_at(day)
    }

    /// Единственная дверь для новой партии: история приобретений
    /// обязана пополняться вместе с партиями, иначе граница владения
    /// разойдётся с журналом.
    #[cfg(test)]
    fn push_lot(&mut self, lot: Lot) {
        self.push_lot_with_settlement(lot, SettlementKnowledge::Unbounded);
    }

    fn push_lot_with_settlement(&mut self, lot: Lot, settlement: SettlementKnowledge) {
        self.acquisitions.observe(lot.acquired);
        self.ownership.observe(lot.quantity, settlement);
        self.lots.push(lot);
    }

    /// Восстановленная партия встаёт в голову очереди FIFO: она старше
    /// всего, что система видела.
    fn insert_restored_lot_with_settlement(&mut self, lot: Lot, settlement: SettlementKnowledge) {
        self.acquisitions.observe(lot.acquired);
        self.ownership.observe(lot.quantity, settlement);
        self.lots.insert(0, lot);
    }

    /// Известная выплата, которую нельзя приписать документированному лоту.
    #[must_use]
    pub const fn unallocated_income(&self) -> Option<Money> {
        self.unallocated_income
    }

    /// Доля известных выплат, пришедшаяся на восстановленное количество.
    #[must_use]
    pub const fn unpriced_income(&self) -> Option<Money> {
        self.unpriced_income
    }

    /// Стоимость приобретений. Вместе с [`Self::released_basis`] образует
    /// проверяемое тождество: приобретено = осталось + списано.
    #[must_use]
    pub const fn acquired_basis(&self) -> Option<Money> {
        self.acquired_basis
    }

    #[must_use]
    pub const fn released_basis(&self) -> Option<Money> {
        self.released_basis
    }

    /// Стоимость непроданных партий.
    pub fn remaining_basis(&self) -> Result<Option<Money>, MoneyError> {
        let Some(first) = self.lots.first() else {
            return Ok(None);
        };
        let amounts: Vec<Money> = self.lots.iter().map(|lot| lot.cost_basis).collect();
        Money::sum(&amounts, first.cost_basis.currency()).map(Some)
    }

    /// Партии для правки внутри модуля.
    ///
    /// Метод-аксессор, а не прямой доступ к приватному полю: правка
    /// партий — операция книги, и её место видно по вызовам.
    fn lots_mut(&mut self) -> &mut [Lot] {
        &mut self.lots
    }

    /// Отметить разрыв в стоимости. Реализованный результат при этом
    /// становится невычислимым: один разрыв делает невычислимым весь
    /// инструмент, а не «почти всё».
    fn mark_basis_gap(&mut self, gap: BasisGap) {
        self.gap = Some(gap);
        self.realized = None;
    }

    /// Прибавить реализованный результат, если он ещё вычислим.
    fn add_realised(&mut self, amount: Money) -> Result<(), MoneyError> {
        if self.gap.is_some() {
            return Ok(());
        }
        self.realized = Some(match self.realized {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    /// Прибавить списанную стоимость: тождество «приобретено = осталось
    /// плюс списано» проверяется инвариантом проекции.
    fn add_released_basis(&mut self, amount: Money) -> Result<(), MoneyError> {
        self.released_basis = Some(match self.released_basis {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    fn add_acquired_basis(&mut self, amount: Money) -> Result<(), MoneyError> {
        self.acquired_basis = Some(match self.acquired_basis {
            Some(previous) => previous.try_add(amount)?,
            None => amount,
        });
        Ok(())
    }

    /// Суммарное количество: партии плюс восстановленный остаток.
    pub fn quantity(&self) -> Result<Quantity, NumericError> {
        self.lots
            .iter()
            .try_fold(self.unpriced.0, |acc, lot| acc.checked_add(lot.quantity.0))
            .map(Quantity)
    }
    /// Добавляет фактическую выплату лотам пропорционально количеству.
    ///
    /// В знаменатель входит и восстановленное количество. Его доля
    /// сохраняется как нераспределённая: у него нет лота, которому можно
    /// приписать выплату.
    fn add_received(&mut self, amount: Money) -> Result<(), LotError> {
        let total_quantity = self.quantity()?.0.inner();
        if total_quantity.is_zero() {
            self.unallocated_income = Some(match self.unallocated_income {
                Some(previous) => previous.try_add(amount)?,
                None => amount,
            });
            return Ok(());
        }

        let mut remaining_amount = amount;
        let mut remaining_quantity = total_quantity;
        if !self.unpriced.0.is_zero() {
            let unpriced_amount = split_basis(amount, self.unpriced.0.inner(), total_quantity)?;
            self.unpriced_income = Some(match self.unpriced_income {
                Some(previous) => previous.try_add(unpriced_amount)?,
                None => unpriced_amount,
            });
            remaining_amount = remaining_amount.try_sub(unpriced_amount)?;
            remaining_quantity = remaining_quantity
                .checked_sub(self.unpriced.0.inner())
                .ok_or(NumericError::Overflow)?;
        }

        for lot in self.lots_mut() {
            let lot_quantity = lot.quantity.0.inner();
            if lot_quantity.is_zero() {
                continue;
            }
            let received = split_basis(remaining_amount, lot_quantity, remaining_quantity)?;
            lot.received_to_date = Some(match lot.received_to_date {
                Some(previous) => previous.try_add(received)?,
                None => received,
            });
            remaining_amount = remaining_amount.try_sub(received)?;
            remaining_quantity = remaining_quantity
                .checked_sub(lot_quantity)
                .ok_or(NumericError::Overflow)?;
        }
        Ok(())
    }

    /// Группирует оставшиеся партии по дате приобретения.
    pub fn cohorts(&self) -> Result<Vec<Cohort>, CohortGap> {
        if !self.unpriced.0.is_zero() {
            return Err(CohortGap::AcquisitionDateUnknown);
        }
        if self.income_kind_unknown {
            return Err(CohortGap::IncomeKindUnknown);
        }
        if self.gap == Some(BasisGap::RestoredWithoutBasis) {
            return Err(CohortGap::RestoredWithoutBasis);
        }

        let mut grouped: BTreeMap<TradeDate, Vec<&Lot>> = BTreeMap::new();
        for lot in &self.lots {
            let acquired = lot.acquired.ok_or(CohortGap::AcquisitionDateUnknown)?;
            grouped.entry(acquired).or_default().push(lot);
        }

        let mut cohorts = Vec::with_capacity(grouped.len());
        for (acquired, lots) in grouped {
            let mut quantity = Dec::zero();
            let acquisition_basis = Self::sum_optional_money(&lots, |lot| lot.acquisition_basis)?;
            let mut cost_basis = Money::zero(lots[0].cost_basis.currency());
            for lot in &lots {
                quantity = quantity
                    .checked_add(lot.quantity.0)
                    .map_err(|_| CohortGap::InconsistentQuantity)?;
                cost_basis = cost_basis
                    .try_add(lot.cost_basis)
                    .map_err(|error| match error {
                        MoneyError::CurrencyMismatch { .. } => {
                            CohortGap::InconsistentCostBasisCurrency
                        }
                        MoneyError::Overflow | MoneyError::Numeric(_) => {
                            CohortGap::InconsistentCostBasisOverflow
                        }
                    })?;
            }
            cohorts.push(Cohort {
                acquired,
                quantity: Quantity(quantity),
                cost_basis,
                acquisition_basis,
                accrued_interest_paid: Self::sum_optional_money(&lots, |lot| {
                    lot.accrued_interest_paid
                })?,
                received_to_date: Self::sum_optional_money(&lots, |lot| lot.received_to_date)?,
            });
        }
        Ok(cohorts)
    }

    fn sum_optional_money<F>(lots: &[&Lot], select: F) -> Result<Option<Money>, CohortGap>
    where
        F: Fn(&Lot) -> Option<Money>,
    {
        let mut total: Option<Money> = None;
        for lot in lots {
            let Some(value) = select(lot) else {
                return Ok(None);
            };
            total = Some(match total {
                Some(previous) => previous.try_add(value).map_err(|error| match error {
                    MoneyError::CurrencyMismatch { .. } => {
                        CohortGap::InconsistentOptionalMoneyCurrency
                    }
                    MoneyError::Overflow | MoneyError::Numeric(_) => {
                        CohortGap::InconsistentOptionalMoneyOverflow
                    }
                })?,
                None => value,
            });
        }
        Ok(total)
    }
}

/// Факты сделки, нужные книге лотов. Отдельная структура, а не восемь
/// аргументов: порог `too-many-arguments-threshold = 6` в `clippy.toml`
/// действует, а подавлять линт запрещено (§15.7).
#[derive(Debug, Clone, Copy, PartialEq)]
struct TradeFacts {
    side: TradeSide,
    instrument: InstrumentId,
    quantity: Quantity,
    gross: Money,
    fee: Option<Money>,
    accrued_interest: Option<Money>,
}

/// Факты амортизации, нужные книге лотов.
#[derive(Debug, Clone, PartialEq)]
struct AmortisationFacts {
    instrument: InstrumentId,
    quantity: Quantity,
    allocation: BasisAllocation,
    compensation: Money,
}
/// Факты замещения, нужные книге лотов.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ConversionFacts {
    predecessor: InstrumentId,
    successor: InstrumentId,
    ratio: Dec,
    quantity_in: Quantity,
    quantity_out: Quantity,
    basis_transfer: BasisTransferRule,
    effective_date: time::Date,
}
/// Данные одной восстановленной позиции: инструмент, количество, стоимость и
/// заявленная дата приобретения. Эти четыре значения описывают саму позицию,
/// поэтому группируются вместе, а событие и знание о расчёте остаются
/// обстоятельствами записи.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RestoreFacts {
    instrument: InstrumentId,
    quantity: Quantity,
    cost_basis: Option<Money>,
    acquired: Option<TradeDate>,
}

/// Данные одного выбытия: ключ лота, количество и выручка. Эти значения
/// составляют одну операцию выбытия и передаются вместе, тогда как событие,
/// правило и знание о расчёте являются её контекстом.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DisposalFacts {
    key: LotKey,
    quantity: Quantity,
    proceeds: Money,
}

/// Книга лотов и применённое правило.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LotBook {
    entries: BTreeMap<LotKey, InstrumentLots>,
    rule_version: LotRuleVersion,
    applied_rule: Option<RuleId>,
    /// Версия правила амортизации. Отдельная от версии списания:
    /// списание лотов — выбор владельца, амортизация — событие выпуска.
    ///
    /// `#[serde(default)]` обязателен: снимки проекций записаны до E3.4
    /// и этого поля не содержат.
    #[serde(default = "default_amortisation_version")]
    amortisation_version: AmortisationRuleVersion,
    settlement_policy: SettlementLagPolicy,
}

/// Версия правила амортизации в снимке, записанном до E3.4.
///
/// Единица, а не «неизвестно»: до E3.4 амортизация не применялась вовсе,
/// поэтому продолжение такого снимка ничего не пересчитывает задним
/// числом — оно применяет правило к фактам, которых раньше не было.
fn default_amortisation_version() -> AmortisationRuleVersion {
    AmortisationRuleVersion(1)
}

impl LotBook {
    #[must_use]
    pub fn new(rule_version: LotRuleVersion) -> Self {
        Self {
            entries: BTreeMap::new(),
            rule_version,
            applied_rule: None,
            amortisation_version: default_amortisation_version(),
            settlement_policy: SettlementLagPolicy::default(),
        }
    }

    /// Книга с явно выбранной версией правила амортизации.
    #[must_use]
    pub fn with_amortisation_version(mut self, version: AmortisationRuleVersion) -> Self {
        self.amortisation_version = version;
        self
    }
    /// Выбрать таблицу полос расчётов для этой книги.
    #[must_use]
    pub fn with_settlement_lag_policy(mut self, policy: SettlementLagPolicy) -> Self {
        self.settlement_policy = policy;
        self
    }

    #[must_use]
    pub const fn amortisation_version(&self) -> AmortisationRuleVersion {
        self.amortisation_version
    }

    #[must_use]
    pub const fn rule_version(&self) -> LotRuleVersion {
        self.rule_version
    }

    /// Идентификатор правила, которым фактически списывались лоты.
    /// Входит в отчёт и в след аудита: без него цифру не воспроизвести (§3.2).
    #[must_use]
    pub const fn applied_rule(&self) -> Option<&RuleId> {
        self.applied_rule.as_ref()
    }

    #[must_use]
    pub fn entry(&self, key: &LotKey) -> Option<&InstrumentLots> {
        self.entries.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&LotKey, &InstrumentLots)> {
        self.entries.iter()
    }

    /// Применение события к книге лотов.
    ///
    /// Диспетчер исчерпывающий: новый тип события обязан сломать сборку
    /// здесь, а не молча не создать лот.
    pub fn apply(&mut self, event: &Event, rules: &RuleRegistry) -> Result<(), LotError> {
        let settlement = self
            .settlement_policy
            .knowledge(&event.dates, event.provenance.parser_version());
        match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
            } => self.apply_trade(
                event,
                TradeFacts {
                    side: *side,
                    instrument: *instrument,
                    quantity: *quantity,
                    gross: *gross,
                    fee: *fee,
                    accrued_interest: *accrued_interest,
                },
                settlement,
                rules,
            ),
            // Утверждения восстановленного начала определяют границу
            // владения: дата события — момент импорта, а не доказательство
            // происхождения позиции.
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis,
                assertions,
            } => {
                let acquired = match (
                    assertions.acquisition_date,
                    assertions.acquisition_date_certainty,
                ) {
                    (Some(day), DateCertainty::Known) => Some(TradeDate(day)),
                    // Оценочная дата не должна становиться датой когорты:
                    // иначе догадка владельца снова выдастся за факт.
                    _ => None,
                };
                let settlement = match acquired {
                    Some(day) => SettlementKnowledge::Exact(day.0),
                    None => {
                        // Оценка не превращается в доказанное начало:
                        // непрерывность владения до открытия журнала
                        // недоказуема в принципе (§3.5).
                        SettlementKnowledge::Unbounded
                    }
                };
                self.restore(
                    event,
                    RestoreFacts {
                        instrument: *instrument,
                        quantity: *quantity,
                        cost_basis: *cost_basis,
                        acquired,
                    },
                    settlement,
                )
            }
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Ok(()),
            EventKind::Income {
                instrument: Some(instrument),
                gross,
                kind,
            } => self.apply_income(event, *instrument, *gross, *kind),
            EventKind::Income {
                instrument: None, ..
            } => Ok(()),
            EventKind::CorporateAction { action } => {
                self.apply_corporate_action(event, action, settlement, rules)
            }
            EventKind::OfferExercise { action } => {
                self.apply_offer_exercise(event, action, settlement, rules)
            }
        }
    }

    fn apply_income(
        &mut self,
        event: &Event,
        instrument: InstrumentId,
        amount: Money,
        kind: Option<IncomeKind>,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match kind {
            Some(IncomeKind::Coupon) => entry.add_received(amount),
            Some(IncomeKind::Dividend | IncomeKind::DepositInterest) => Ok(()),
            None => {
                entry.income_kind_unknown = true;
                Ok(())
            }
        }
    }

    /// Стоимость приобретения включает комиссию и **не включает НКД**:
    /// накопленный купонный доход возвращается купоном, а не продажей,
    /// поэтому он не является стоимостью бумаги (§7.2). Налоговая
    /// стоимость по ст. 214.1 считается иначе и появится в E5 —
    /// поэтому она и версионирована правилом.
    fn apply_trade(
        &mut self,
        event: &Event,
        trade: TradeFacts,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let TradeFacts {
            side,
            instrument,
            quantity,
            gross,
            fee,
            accrued_interest,
        } = trade;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        match side {
            TradeSide::Buy => {
                let basis = match fee {
                    Some(f) => gross.try_add(f)?,
                    None => gross,
                };
                let entry = self.entries.entry(key).or_default();
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.push_lot_with_settlement(
                    Lot {
                        // Идентификатор лота выводится из события приобретения:
                        // ядро чисто, случайных идентификаторов в нём быть не может,
                        // иначе повторная проекция того же журнала дала бы другой
                        // результат (§3.1, §15.3).
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired: event.dates.trade,
                        quantity,
                        // Отсутствующий НКД оставляем неизвестным, а не нулём.
                        accrued_interest_paid: accrued_interest,
                        received_to_date: None,
                        cost_basis: basis,
                        acquisition_basis: Some(basis),
                        // Номинал сюда придёт из справочника в E3.4;
                        // подставлять ноль запрещено (§4.9).
                        principal: PrincipalState::Unknown,
                    },
                    settlement,
                );
                Ok(())
            }
            TradeSide::Sell => {
                let proceeds = match fee {
                    Some(f) => gross.try_sub(f)?,
                    None => gross,
                };
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity,
                        proceeds,
                    },
                    rules,
                    settlement,
                )
            }
        }
    }

    /// Корпоративное действие по бумаге (§4.7).
    ///
    /// Диспетчер исчерпывающий: новый член семейства обязан сломать
    /// сборку здесь, а не молча оставить лоты прежними.
    fn apply_corporate_action(
        &mut self,
        event: &Event,
        action: &CorporateAction,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        match action {
            CorporateAction::PartialRedemption {
                instrument,
                quantity,
                basis_allocation,
                compensation,
                ..
            } => self.apply_amortisation(
                event,
                AmortisationFacts {
                    instrument: *instrument,
                    quantity: *quantity,
                    allocation: basis_allocation.clone(),
                    compensation: *compensation,
                },
                rules,
            ),
            // Погашение возвращает номинал целиком и выводит бумагу
            // из позиции: это выбытие всей позиции, и считается оно тем
            // же путём, что продажа. Порядок списания при полном выбытии
            // безразличен, поэтому выбор правила владельцем ничего
            // не меняет — но след аудита остаётся тот же.
            CorporateAction::Redemption {
                instrument,
                quantity,
                compensation,
                ..
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                self.require_whole_position(event, key, *quantity)?;
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity: *quantity,
                        proceeds: *compensation,
                    },
                    rules,
                    settlement,
                )
            }
            CorporateAction::Conversion {
                predecessor,
                successor,
                ratio,
                quantity_in,
                quantity_out,
                basis_transfer,
                effective_date,
                ..
            } => self.apply_conversion(
                event,
                ConversionFacts {
                    predecessor: *predecessor,
                    successor: *successor,
                    ratio: *ratio,
                    quantity_in: *quantity_in,
                    quantity_out: *quantity_out,
                    basis_transfer: *basis_transfer,
                    effective_date: *effective_date,
                },
                settlement,
            ),
        }
    }

    /// Исполнение оферты (§3.5).
    fn apply_offer_exercise(
        &mut self,
        event: &Event,
        action: &OfferExerciseAction,
        settlement: SettlementKnowledge,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        match action {
            // Заявка и её отзыв лотов не двигают: бумага остаётся
            // у владельца, пока выкуп не состоялся. Их состояние ведёт
            // отдельная проекция `super::offers::OfferBook`.
            OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => Ok(()),
            // Выкуп — выбытие: бумага уходит, деньги приходят.
            OfferExerciseAction::Settled {
                instrument,
                quantity,
                gross,
                fee,
                accrued_interest,
                ..
            } => {
                let key = LotKey {
                    account: event.account,
                    instrument: *instrument,
                };
                let mut proceeds = *gross;
                if let Some(interest) = accrued_interest {
                    proceeds = proceeds.try_add(*interest)?;
                }
                if let Some(fee) = fee {
                    proceeds = proceeds.try_sub(*fee)?;
                }
                self.dispose(
                    event,
                    DisposalFacts {
                        key,
                        quantity: *quantity,
                        proceeds,
                    },
                    rules,
                    settlement,
                )
            }
        }
    }

    /// Амортизация: остаток номинала уменьшается, количество — нет (§6.5).
    ///
    /// Целимся по паре «счёт и бумага»: [`LotKey`] намеренно не различает
    /// место хранения, и custody из события — факт о выплате, а не ключ
    /// выборки лотов.
    fn apply_amortisation(
        &mut self,
        event: &Event,
        facts: AmortisationFacts,
        rules: &RuleRegistry,
    ) -> Result<(), LotError> {
        let key = LotKey {
            account: event.account,
            instrument: facts.instrument,
        };
        self.require_whole_position(event, key, facts.quantity)?;
        let rule = rules.amortisation_rule(self.amortisation_version).ok_or(
            LotError::UnknownAmortisationRule {
                version: self.amortisation_version,
            },
        )?;
        let entry = self
            .entries
            .get(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: facts.instrument,
            })?;

        // Считаем на копии и подменяем целиком: иначе отказ на втором
        // лоте оставил бы первый уже изменённым, а половина применённого
        // факта хуже неприменённого.
        let mut next = entry.clone();
        let mut returned_total = Money::zero(facts.compensation.currency());
        match facts.allocation {
            BasisAllocation::Known { share, .. } => {
                for lot in next.lots_mut() {
                    let returned = rule.basis_returned(lot, share)?;
                    lot.cost_basis = lot.cost_basis.try_sub(returned)?;
                    returned_total = returned_total.try_add(returned)?;
                }
            }
            // Доля неизвестна — факт всё равно применяется, а
            // реализованный результат становится невычислимым (§4.9).
            // Разносить было не по чему, а отказать в приёме факта
            // нельзя: деньги пришли.
            BasisAllocation::Unknown(_) => {
                next.mark_basis_gap(BasisGap::AmortisationAllocationUnknown);
            }
        }
        next.add_received(facts.compensation)?;
        if !returned_total.is_zero() {
            next.add_released_basis(returned_total)?;
        }
        // Возврат собственного капитала доходом не является: при
        // компенсации, равной возвращённой стоимости, реализуется ноль.
        next.add_realised(facts.compensation.try_sub(returned_total)?)?;
        self.entries.insert(key, next);
        Ok(())
    }

    /// Замещение: партии предшественника становятся партиями преемника.
    fn apply_conversion(
        &mut self,
        event: &Event,
        facts: ConversionFacts,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let from = LotKey {
            account: event.account,
            instrument: facts.predecessor,
        };
        self.require_whole_position(event, from, facts.quantity_in)?;
        let quantity_in_delta = Quantity(facts.quantity_in.0.checked_neg()?);
        let source = self
            .entries
            .get(&from)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: facts.predecessor,
            })?
            .clone();

        let mut moved = Vec::with_capacity(source.lots().len());
        let mut assigned = Dec::zero();
        let mut carried = Vec::with_capacity(source.lots().len());
        for (index, lot) in source.lots().iter().enumerate() {
            let last = index + 1 == source.lots().len();
            // Последней партии достаётся остаток: дробь округляли
            // на уровне всего замещения, и раскладывать её обратно
            // по партиям нечем. Так сумма партий равна количеству
            // из факта точно, а не почти.
            let quantity = if last {
                Quantity(facts.quantity_out.0.checked_sub(assigned)?)
            } else {
                Quantity(lot.quantity.0.checked_mul(facts.ratio)?)
            };
            assigned = assigned.checked_add(quantity.0)?;
            carried.push(lot.cost_basis);
            moved.push(Lot {
                id: lot.id,
                instrument: facts.successor,
                acquired: match facts.basis_transfer {
                    // Срок владения переходит целиком: замещение
                    // не является приобретением (§16.1).
                    BasisTransferRule::CarryOver => lot.acquired,
                    // Замещение приравнено к продаже и покупке.
                    BasisTransferRule::Restart => Some(TradeDate(facts.effective_date)),
                },
                quantity,
                // Стоимость переносится как есть. Компенсация дробей
                // из неё **не** вычитается: как она влияет на базу —
                // правило E5, и решать за него часть 1 не вправе.
                cost_basis: lot.cost_basis,
                acquisition_basis: lot.acquisition_basis,
                accrued_interest_paid: lot.accrued_interest_paid,
                received_to_date: lot.received_to_date,
                // Номинал преемника — свойство другого выпуска, и
                // вывести его из номинала предшественника нечем.
                // Подставить прежний значило бы выдумать (§4.9).
                principal: PrincipalState::Unknown,
            });
        }
        let currency = match carried.first() {
            Some(first) => first.currency(),
            // Позиция без партий: замещать нечего, но и ошибки нет —
            // количество уже сверено с фактом.
            None => return Ok(()),
        };
        let carried_total = Money::sum(&carried, currency)?;

        let mut source = source;
        source.lots.clear();
        source.add_released_basis(carried_total)?;
        source.ownership.observe(quantity_in_delta, settlement);
        self.entries.insert(from, source);

        let to = LotKey {
            account: event.account,
            instrument: facts.successor,
        };
        let target = self.entries.entry(to).or_default();
        target.add_acquired_basis(carried_total)?;
        target.ownership.observe(facts.quantity_out, settlement);
        for lot in moved {
            // Перенос лота не является новым приобретением: старую
            // AcquisitionHistory сохраняем отдельно от дельты владения.
            target.acquisitions.observe(lot.acquired);
            target.lots.push(lot);
        }
        Ok(())
    }

    /// Корпоративное действие касается всей позиции по бумаге на счёте.
    ///
    /// Расхождение — брак источника, а не повод уменьшить номинал
    /// пропорционально: масштабирование выдало бы испорченные данные
    /// за корректный расчёт.
    fn require_whole_position(
        &self,
        event: &Event,
        key: LotKey,
        declared: Quantity,
    ) -> Result<(), LotError> {
        let entry = self
            .entries
            .get(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;
        let held = entry.quantity()?;
        if held == declared {
            Ok(())
        } else {
            Err(LotError::QuantityMismatch { held, declared })
        }
    }

    fn restore(
        &mut self,
        event: &Event,
        facts: RestoreFacts,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let RestoreFacts {
            instrument,
            quantity,
            cost_basis,
            acquired,
        } = facts;
        let key = LotKey {
            account: event.account,
            instrument,
        };
        let entry = self.entries.entry(key).or_default();
        match cost_basis {
            // Восстановленная партия старше всего, что система видела,
            // поэтому встаёт в голову очереди FIFO, а не в хвост.
            Some(basis) => {
                entry.acquired_basis = Some(match entry.acquired_basis {
                    Some(previous) => previous.try_add(basis)?,
                    None => basis,
                });
                entry.insert_restored_lot_with_settlement(
                    Lot {
                        id: LotId(event.id.inner()),
                        instrument,
                        acquired,
                        quantity,
                        accrued_interest_paid: None,
                        received_to_date: None,
                        cost_basis: basis,
                        acquisition_basis: Some(basis),
                        principal: PrincipalState::Unknown,
                    },
                    settlement,
                );
            }
            None => {
                entry.unpriced = Quantity(entry.unpriced.0.checked_add(quantity.0)?);
                entry.gap = Some(BasisGap::RestoredWithoutBasis);
                // Даты у восстановленного количества нет, а приобретено
                // оно раньше всего, что система видела: граница владения
                // по этой паре недоказуема и после того, как количество
                // спишется.
                entry.acquisitions.observe(None);
                entry.ownership.observe(quantity, settlement);
            }
        }
        Ok(())
    }

    fn dispose(
        &mut self,
        event: &Event,
        facts: DisposalFacts,
        rules: &RuleRegistry,
        settlement: SettlementKnowledge,
    ) -> Result<(), LotError> {
        let DisposalFacts {
            key,
            quantity,
            proceeds,
        } = facts;
        let delta = Quantity(quantity.0.checked_neg()?);
        let rule = rules
            .disposal_rule(self.rule_version)
            .ok_or(LotError::UnknownRule {
                version: self.rule_version,
            })?;
        let entry = self
            .entries
            .get_mut(&key)
            .ok_or(LotError::SaleWithoutPosition {
                event: event.id,
                instrument: key.instrument,
            })?;

        // Восстановленное количество списывается первым: оно приобретено
        // раньше всего, что система наблюдала. Стоимости у него нет,
        // поэтому реализованный результат по инструменту становится
        // невычислимым — но количество списывается честно.
        let from_unpriced = entry.unpriced.0.min(quantity.0);
        if !from_unpriced.is_zero() {
            entry.unpriced = Quantity(entry.unpriced.0.checked_sub(from_unpriced)?);
            entry.realized = None;
            entry.gap = Some(BasisGap::RestoredWithoutBasis);
        }
        let left = quantity.0.checked_sub(from_unpriced)?;
        if left.is_zero() {
            entry.ownership.observe(delta, settlement);
            return Ok(());
        }

        let result: DisposalResult = rule.apply(&DisposalInput {
            lots: entry.lots.clone(),
            quantity: Quantity(left),
        })?;
        entry.lots = result.remaining.clone();
        entry.released_basis = Some(match entry.released_basis {
            Some(previous) => previous.try_add(result.basis_released)?,
            None => result.basis_released,
        });
        self.applied_rule = Some(result.rule.clone());

        // Реализованный результат до налога: выручка минус списанная
        // стоимость. Он не суммируется с невычислимым: один разрыв делает
        // невычислимым весь инструмент, а не «почти всё».
        if entry.gap.is_none() {
            let realized = proceeds.try_sub(result.basis_released)?;
            entry.realized = Some(match entry.realized {
                Some(previous) => previous.try_add(realized)?,
                None => realized,
            });
        }
        entry.ownership.observe(delta, settlement);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::CustodyId;
    use crate::money::PerUnitAmount;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::Date;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    // --- корпоративные действия и оферта ---

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    /// Позиция по облигации, собранная напрямую: событие покупки
    /// номинала не знает, а тесту он нужен как исходное состояние.
    struct Bond {
        account: AccountId,
        instrument: InstrumentId,
        custody: CustodyId,
    }
    fn known_allocation(returned: &str) -> BasisAllocation {
        BasisAllocation::Known {
            share: crate::rules::ReturnedShare::new(
                dec(returned)
                    .checked_div(dec("1000"))
                    .expect("номинал тестовой облигации ненулевой"),
            )
            .expect("доля в пределах инварианта"),
            evidence: crate::event::allocation::AllocationEvidence {
                inputs_hash: crate::event::allocation::AllocationInputsHash::new("a".repeat(64))
                    .expect("хеш входов"),
                knowledge_as_of: time::OffsetDateTime::UNIX_EPOCH,
                algorithm_version: crate::event::allocation::AllocationAlgorithmVersion(1),
            },
        }
    }

    impl Bond {
        fn new() -> Self {
            Self {
                account: AccountId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
            }
        }

        fn key(&self) -> LotKey {
            LotKey {
                account: self.account,
                instrument: self.instrument,
            }
        }

        fn amortisation(&self, units: i64, returned: &str, compensation: i64) -> Event {
            self.amortisation_with_allocation(
                units,
                returned,
                compensation,
                known_allocation(returned),
            )
        }

        fn unknown_amortisation(&self, units: i64, returned: &str, compensation: i64) -> Event {
            self.amortisation_with_allocation(
                units,
                returned,
                compensation,
                BasisAllocation::default(),
            )
        }

        fn amortisation_with_allocation(
            &self,
            units: i64,
            returned: &str,
            compensation: i64,
            basis_allocation: BasisAllocation,
        ) -> Event {
            event_with(
                self.account,
                date!(2026 - 06 - 15),
                5,
                EventKind::CorporateAction {
                    action: CorporateAction::PartialRedemption {
                        instrument: self.instrument,
                        custody: self.custody,
                        quantity: qty(units),
                        principal_returned_per_unit: per_unit(returned),
                        compensation: rub(compensation),
                        effective_date: date!(2026 - 06 - 15),
                        record_date: None,
                        grounds: None,
                        basis_allocation,
                    },
                },
                vec![Leg::principal(
                    self.account,
                    self.instrument,
                    rub(compensation),
                )],
            )
        }
    }

    fn bond_lot(bond: &Bond, units: i64, principal: PrincipalState, basis: i64) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument: bond.instrument,
            acquired: Some(crate::dates::TradeDate(date!(2024 - 03 - 01))),
            quantity: qty(units),
            cost_basis: rub(basis),
            acquisition_basis: Some(rub(basis)),
            accrued_interest_paid: None,
            received_to_date: None,
            principal,
        }
    }

    fn book_with_lots(bond: &Bond, lots: Vec<Lot>) -> LotBook {
        let mut book = LotBook::new(LotRuleVersion(1));
        let acquired: Vec<Money> = lots.iter().map(|lot| lot.cost_basis).collect();
        let entry = book.entries.entry(bond.key()).or_default();
        entry.acquired_basis = Money::sum(&acquired, CurrencyCode::Rub).ok();
        for lot in lots {
            entry.push_lot(lot);
        }
        book
    }

    fn remaining_principal(entry: &InstrumentLots) -> Option<PerUnitAmount> {
        entry.lots().first()?.principal.remaining_per_unit()
    }

    fn лот_с_датой(instrument: InstrumentId, acquired: Option<Date>) -> Lot {
        Lot {
            id: LotId::new_random(),
            instrument,
            acquired: acquired.map(TradeDate),
            quantity: qty(10),
            cost_basis: rub(100_000),
            acquisition_basis: Some(rub(100_000)),
            accrued_interest_paid: None,
            received_to_date: None,
            principal: PrincipalState::Unknown,
        }
    }

    #[test]
    fn граница_владения_берёт_самую_раннюю_дату_приобретения() {
        // Партии кладутся `push_lot`, а не присваиванием: история
        // приобретений пополняется только через него, и тест, минующий
        // его, проверял бы фикстуру, а не книгу лотов.
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots::default();
        entry.push_lot(лот_с_датой(
            instrument,
            Some(date!(2025 - 07 - 01)),
        ));
        entry.push_lot(лот_с_датой(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));

        assert_eq!(
            entry.earliest_acquired(),
            Some(TradeDate(date!(2024 - 03 - 01)))
        );
    }

    #[test]
    fn партия_без_даты_приобретения_не_даёт_провести_границу_владения() {
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots::default();
        entry.push_lot(лот_с_датой(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));
        entry.push_lot(лот_с_датой(instrument, None));

        assert_eq!(entry.earliest_acquired(), None);
    }

    #[test]
    fn восстановленное_количество_не_даёт_провести_границу_владения() {
        // Оно приобретено раньше всего, что система видела, и даты у
        // него нет: любая граница по оставшимся партиям была бы позже
        // настоящей и скрыла бы пропуск.
        let instrument = InstrumentId::new_random();
        let mut entry = InstrumentLots {
            unpriced: qty(5),
            ..Default::default()
        };
        entry.push_lot(лот_с_датой(
            instrument,
            Some(date!(2024 - 03 - 01)),
        ));

        assert_eq!(entry.earliest_acquired(), None);
    }

    #[test]
    fn граница_владения_не_поднимается_после_выбытия_ранней_партии() {
        // Купили в январе, купили в апреле, продали январскую партию.
        // Граница обязана остаться январской: бумага в марте была
        // на руках, и пропущенный за март купон надо назвать.
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let январь = Trade {
            account,
            instrument,
            day: date!(2026 - 01 - 10),
            units: 10,
            gross: 100_000,
        };
        let апрель = Trade {
            day: date!(2026 - 04 - 10),
            ..январь
        };
        let продажа = Trade {
            day: date!(2026 - 07 - 10),
            gross: 120_000,
            ..январь
        };
        book.apply(&dated_buy(&январь, 1), &rules).unwrap();
        book.apply(&dated_buy(&апрель, 2), &rules).unwrap();
        book.apply(&sell(&продажа, 3), &rules).unwrap();

        let entry = book.entry(&key(&январь)).unwrap();
        assert_eq!(
            entry.lots().len(),
            1,
            "январская партия должна быть списана"
        );
        assert_eq!(
            entry.earliest_acquired(),
            Some(TradeDate(date!(2026 - 01 - 10)))
        );
    }

    #[test]
    fn выбытие_партии_без_даты_не_делает_границу_владения_известной() {
        // Партия без даты приобретена неизвестно когда, и продажа
        // этого не проясняет. Признать границу апрельской значило бы
        // объявить известным то, чего журнал не говорит (§4.9).
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let без_даты = Trade {
            account,
            instrument,
            day: date!(2026 - 01 - 10),
            units: 10,
            gross: 100_000,
        };
        let апрель = Trade {
            day: date!(2026 - 04 - 10),
            ..без_даты
        };
        let продажа = Trade {
            day: date!(2026 - 07 - 10),
            gross: 120_000,
            ..без_даты
        };
        book.apply(&buy(&без_даты, 1), &rules).unwrap();
        book.apply(&dated_buy(&апрель, 2), &rules).unwrap();
        book.apply(&sell(&продажа, 3), &rules).unwrap();

        let entry = book.entry(&key(&без_даты)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.earliest_acquired(), None);
    }

    fn known(original: &str, remaining: &str) -> PrincipalState {
        PrincipalState::known(per_unit(original), per_unit(remaining)).unwrap()
    }

    #[test]
    fn amortisation_leaves_the_quantity_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(10));
    }

    #[test]
    fn an_amortisation_for_a_different_quantity_is_an_error_not_a_scaling() {
        // Амортизация касается всех бумаг на счёте. Несовпадение — брак
        // источника, а не повод уменьшить номинал пропорционально.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );

        assert!(matches!(
            book.apply(&bond.amortisation(4, "200", 80_000), &rules),
            Err(LotError::QuantityMismatch { .. })
        ));
    }

    #[test]
    fn an_amortisation_returning_exactly_the_basis_realises_nothing() {
        // §6.5: возврат собственного капитала доходом не является.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.realized(), Some(rub(0)));
        assert_eq!(entry.released_basis(), Some(rub(200_000)));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(800_000)));
        assert_eq!(
            entry.lots()[0].acquisition_basis,
            Some(rub(1_000_000)),
            "амортизация не уменьшает историческую стоимость"
        );
    }

    #[test]
    fn amortisation_reduces_current_basis_but_preserves_historical_purchase_cost() {
        // Пожизненный поток включает уже полученные 200 и будущие 800.
        // Если знаменатель взять из уменьшенного cost_basis (800 вместо
        // исторических 1000), HPR ошибочно выйдет 25 % вместо 0 %.
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let lot = &book.entry(&bond.key()).unwrap().lots()[0];
        assert_eq!(lot.cost_basis, rub(800_000));
        assert_eq!(lot.acquisition_basis, Some(rub(1_000_000)));
    }

    #[test]
    fn an_amortisation_paying_above_the_returned_basis_realises_the_difference() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        // Куплена с дисконтом: стоимость 900 000 при номинале 1000 × 10.
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 900_000)],
        );

        book.apply(&bond.amortisation(10, "200", 200_000), &rules)
            .unwrap();

        // Возвращена пятая часть стоимости — 180 000; выплачено 200 000.
        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.released_basis(), Some(rub(180_000)));
        assert_eq!(entry.realized(), Some(rub(20_000)));
    }

    #[test]
    fn an_unknown_allocation_records_a_basis_gap_instead_of_failing() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );

        book.apply(&bond.unknown_amortisation(10, "200", 200_000), &rules)
            .unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.gap(), Some(BasisGap::AmortisationAllocationUnknown));
        assert_eq!(entry.realized(), None);
        // Количество и стоимость не тронуты: считать было не от чего.
        assert_eq!(entry.quantity().unwrap(), qty(10));
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(1_000_000)));
    }

    #[test]
    fn an_amortisation_on_another_account_leaves_this_book_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );
        let elsewhere = Bond {
            account: AccountId::new_random(),
            instrument: bond.instrument,
            custody: bond.custody,
        };

        // Позиции на том счёте нет вовсе: факт к этой книге не относится.
        assert!(matches!(
            book.apply(&elsewhere.amortisation(10, "200", 200_000), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
        assert_eq!(
            remaining_principal(book.entry(&bond.key()).unwrap()),
            Some(per_unit("1000"))
        );
    }

    #[test]
    fn a_redemption_empties_the_position_and_releases_the_whole_basis() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "800"), 800_000)],
        );
        let redemption = event_with(
            bond.account,
            date!(2026 - 12 - 15),
            6,
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument: bond.instrument,
                    custody: bond.custody,
                    quantity: qty(10),
                    principal_returned_per_unit: per_unit("800"),
                    compensation: rub(800_000),
                    effective_date: date!(2026 - 12 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            vec![
                Leg::principal(bond.account, bond.instrument, rub(800_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-10)),
            ],
        );

        book.apply(&redemption, &rules).unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(0));
        assert_eq!(entry.released_basis(), Some(rub(800_000)));
        assert_eq!(entry.realized(), Some(rub(0)));
    }

    #[test]
    fn a_submitted_offer_leaves_the_lots_alone() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );
        let before = book.clone();
        let submitted = event_with(
            bond.account,
            date!(2026 - 07 - 01),
            7,
            EventKind::OfferExercise {
                action: crate::event::offer::OfferExerciseAction::Submitted {
                    submission: crate::event::offer::OfferSubmissionId::new_random(),
                    window: crate::event::offer::OfferWindowId::new_random(),
                    instrument: bond.instrument,
                    quantity: qty(10),
                },
            },
            Vec::new(),
        );

        book.apply(&submitted, &rules).unwrap();
        assert_eq!(book, before);
    }

    #[test]
    fn a_settled_offer_disposes_the_bought_back_quantity() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![bond_lot(&bond, 10, known("1000", "1000"), 1_000_000)],
        );
        let settled = event_with(
            bond.account,
            date!(2026 - 07 - 15),
            8,
            EventKind::OfferExercise {
                action: crate::event::offer::OfferExerciseAction::Settled {
                    submission: crate::event::offer::OfferSubmissionId::new_random(),
                    instrument: bond.instrument,
                    custody: bond.custody,
                    quantity: qty(4),
                    gross: rub(420_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            vec![
                Leg::cash(bond.account, rub(420_000)),
                Leg::security(bond.account, bond.custody, bond.instrument, qty(-4)),
            ],
        );

        book.apply(&settled, &rules).unwrap();

        let entry = book.entry(&bond.key()).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(6));
        assert_eq!(entry.released_basis(), Some(rub(400_000)));
        assert_eq!(entry.realized(), Some(rub(20_000)));
    }

    struct Trade {
        account: AccountId,
        instrument: InstrumentId,
        day: Date,
        units: i64,
        gross: i64,
    }

    fn buy(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(-(trade.gross + 10_000));
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(trade.units),
                ),
            ],
        )
    }

    fn dated_buy(trade: &Trade, sequence: u32) -> Event {
        let mut event = buy(trade, sequence);
        event.dates.trade = Some(TradeDate(trade.day));
        event
    }

    fn sell(trade: &Trade, sequence: u32) -> Event {
        let fee = rub(10_000);
        let settlement = rub(trade.gross - 10_000);
        event_with(
            trade.account,
            trade.day,
            sequence,
            EventKind::Trade {
                side: TradeSide::Sell,
                instrument: trade.instrument,
                quantity: qty(trade.units),
                gross: rub(trade.gross),
                fee: Some(fee),
                accrued_interest: None,
            },
            vec![
                Leg::cash(trade.account, settlement),
                Leg::security(
                    trade.account,
                    CustodyId::new_random(),
                    trade.instrument,
                    qty(-trade.units),
                ),
            ],
        )
    }

    fn key(trade: &Trade) -> LotKey {
        LotKey {
            account: trade.account,
            instrument: trade.instrument,
        }
    }

    fn sample_trade() -> Trade {
        Trade {
            account: AccountId::new_random(),
            instrument: InstrumentId::new_random(),
            day: date!(2025 - 03 - 01),
            units: 100,
            gross: 1_000_000,
        }
    }

    #[test]
    fn a_purchase_creates_a_lot_including_the_fee() {
        // Комиссия входит в стоимость приобретения; НКД — нет (§7.2).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots().len(), 1);
        assert_eq!(entry.lots()[0].cost_basis, rub(1_010_000));
        assert_eq!(entry.lots()[0].acquisition_basis, Some(rub(1_010_000)));
        assert_eq!(entry.quantity().unwrap(), qty(100));
    }

    #[test]
    fn lot_identity_comes_from_the_acquisition_event_not_from_randomness() {
        // Ядро чисто: повторная проекция того же журнала обязана дать
        // те же идентификаторы лотов, иначе снимки несравнимы (§3.1).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let event = buy(&trade, 1);
        let mut first = LotBook::new(LotRuleVersion(1));
        let mut second = LotBook::new(LotRuleVersion(1));
        first.apply(&event, &rules).unwrap();
        second.apply(&event, &rules).unwrap();
        assert_eq!(
            first.entry(&key(&trade)).unwrap().lots()[0].id,
            second.entry(&key(&trade)).unwrap().lots()[0].id
        );
    }

    #[test]
    fn a_partial_sale_releases_basis_and_records_realized_result() {
        // Куплено 100 за 1 010 000, продано 40 за 500 000 минус комиссия.
        // Списанная стоимость: 1 010 000 * 40 / 100 = 404 000.
        // Реализовано: 490 000 − 404 000 = 86 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.quantity().unwrap(), qty(60));
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
        assert_eq!(entry.realized(), Some(rub(86_000)));
        assert_eq!(
            book.applied_rule().map(|r| r.0.as_str()),
            Some("fifo/214.1/v1")
        );
    }

    #[test]
    fn remaining_basis_completes_the_identity_acquired_equals_remaining_plus_released() {
        // Тождество §6.3 в его денежной части. Ожидания посчитаны вручную:
        // куплено 100 за 1 010 000, продано 40 — списано 404 000,
        // значит на непроданных 60 бумагах осталось 606 000.
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert_eq!(
            book.entry(&key(&trade)).and_then(|entry| entry
                .remaining_basis()
                .expect("пустая книга не считает стоимость")),
            None
        );

        book.apply(&buy(&trade, 1), &rules).unwrap();
        let partial = Trade {
            units: 40,
            gross: 500_000,
            day: date!(2025 - 06 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.remaining_basis().unwrap(), Some(rub(606_000)));
        assert_eq!(
            entry.acquired_basis(),
            Some(rub(1_010_000)),
            "приобретено = осталось + списано"
        );
        assert_eq!(entry.released_basis(), Some(rub(404_000)));
    }

    #[test]
    fn a_restored_position_without_basis_does_not_become_a_zero_cost_lot() {
        // Нулевая стоимость означала бы прибыль, равную всей выручке (§4.9).
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let mut restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions {
                    acquisition_date: Some(date!(2024 - 01 - 02)),
                    acquisition_date_certainty: crate::event::kind::DateCertainty::Known,
                    ..crate::event::kind::OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // Точная дата расчёта позволяет проверить, что восстановленное
        // количество участвует во владении так же, как документированный лот.
        restored.dates.settled = Some(crate::dates::SettledDate(date!(2024 - 01 - 02)));
        book.apply(&restored, &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unpriced(), qty(50));
        assert_eq!(entry.gap(), Some(BasisGap::RestoredWithoutBasis));
        assert_eq!(entry.ownership_at(date!(2024 - 01 - 03)), Ownership::Owned);

        // Продажа из восстановленного количества уменьшает позицию,
        // но реализованный результат остаётся невычислимым.
        let partial = Trade {
            units: 20,
            gross: 300_000,
            day: date!(2025 - 02 - 01),
            ..trade
        };
        book.apply(&sell(&partial, 2), &rules).unwrap();
        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(30));
        assert_eq!(entry.realized(), None);
    }
    #[test]
    fn a_known_opening_acquisition_date_proves_ownership_after_the_claimed_date() {
        use crate::event::kind::{DateCertainty, OpeningAssertions};

        let trade = sample_trade();
        let claimed = date!(2021 - 05 - 01);
        let mut restored = event_with(
            trade.account,
            date!(2026 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: OpeningAssertions {
                    acquisition_date: Some(claimed),
                    acquisition_date_certainty: DateCertainty::Known,
                    ..OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // Дата события — день импорта; она не доказывает происхождение позиции.
        restored.dates.trade = Some(crate::dates::TradeDate(date!(2026 - 01 - 01)));

        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.ownership_at(date!(2022 - 06 - 15)), Ownership::Owned);
        assert_eq!(
            entry.lots()[0].acquired,
            Some(crate::dates::TradeDate(claimed))
        );
    }

    #[test]
    fn an_estimated_opening_acquisition_date_does_not_prove_ownership() {
        use crate::event::kind::{DateCertainty, OpeningAssertions};

        let trade = sample_trade();
        let mut restored = event_with(
            trade.account,
            date!(2026 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: OpeningAssertions {
                    acquisition_date: Some(date!(2021 - 05 - 01)),
                    acquisition_date_certainty: DateCertainty::Estimated,
                    ..OpeningAssertions::default()
                },
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        // Даже правдоподобная дата импорта не заменяет доказательство начала.
        restored.dates.trade = Some(crate::dates::TradeDate(date!(2021 - 05 - 01)));

        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();

        assert_eq!(
            book.entry(&key(&trade))
                .unwrap()
                .ownership_at(date!(2022 - 06 - 15)),
            Ownership::Unknown
        );
    }

    #[test]
    fn selling_an_instrument_never_held_is_an_error() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        assert!(matches!(
            book.apply(&sell(&trade, 1), &rules),
            Err(LotError::SaleWithoutPosition { .. })
        ));
    }

    #[test]
    fn an_unknown_rule_version_is_an_error_not_a_silent_fallback() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(99));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        assert!(matches!(
            book.apply(&sell(&trade, 2), &rules),
            Err(LotError::UnknownRule { .. })
        ));
    }
    #[test]
    fn the_book_exposes_its_entries_and_the_cost_of_acquisitions() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();

        assert_eq!(book.iter().count(), 1, "книга обязана отдавать записи");
        let (found_key, entry) = book.iter().next().unwrap();
        assert_eq!(*found_key, key(&trade));
        // Приобретено = тело + комиссия; вместе со списанным образует
        // проверяемое тождество сохранения стоимости.
        assert_eq!(entry.acquired_basis(), Some(rub(1_010_000)));
        assert_eq!(entry.released_basis(), None);
        assert_eq!(book.rule_version(), LotRuleVersion(1));
    }

    #[test]
    fn the_basis_gap_has_a_machine_readable_code() {
        // Код уходит в API: агент разбирает его, а не текст.
        assert_eq!(
            BasisGap::RestoredWithoutBasis.code(),
            "restored_without_basis"
        );
    }
    fn set_accrued_interest(event: &mut Event, value: Option<Money>) {
        match &mut event.kind {
            EventKind::Trade {
                accrued_interest, ..
            } => *accrued_interest = value,
            _ => panic!("expected trade event"),
        }
    }

    #[test]
    fn a_bond_purchase_records_paid_accrued_interest_and_preserves_unknown() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut with_interest = buy(&trade, 1);
        set_accrued_interest(&mut with_interest, Some(rub(12_345)));
        let mut with = LotBook::new(LotRuleVersion(1));
        with.apply(&with_interest, &rules).unwrap();
        assert_eq!(
            with.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            Some(rub(12_345))
        );

        let mut without = LotBook::new(LotRuleVersion(1));
        without.apply(&buy(&trade, 1), &rules).unwrap();
        assert_eq!(
            without.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            None
        );
    }

    #[test]
    fn an_opening_position_never_invents_paid_accrued_interest() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: Some(rub(500_000)),
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );

        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&restored, &rules).unwrap();
        assert_eq!(
            book.entry(&key(&trade)).unwrap().lots()[0].accrued_interest_paid,
            None
        );
    }
    #[test]
    fn a_coupon_is_distributed_between_lots_by_quantity() {
        let trade = sample_trade();
        let first = Trade {
            day: date!(2025 - 03 - 01),
            units: 30,
            gross: 300_000,
            ..trade
        };
        let second = Trade {
            day: date!(2025 - 04 - 01),
            units: 70,
            gross: 700_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&first, 1), &rules).unwrap();
        book.apply(&buy(&second, 2), &rules).unwrap();
        let coupon = event_with(
            trade.account,
            date!(2025 - 05 - 01),
            3,
            EventKind::Income {
                instrument: Some(trade.instrument),
                gross: rub(101),
                kind: Some(crate::event::kind::IncomeKind::Coupon),
            },
            vec![Leg::cash(trade.account, rub(101))],
        );

        book.apply(&coupon, &rules).unwrap();

        let lots = &book.entry(&key(&trade)).unwrap().lots;
        assert_eq!(lots[0].received_to_date, Some(rub(30)));
        assert_eq!(lots[1].received_to_date, Some(rub(71)));
        assert_eq!(
            lots.iter()
                .filter_map(|lot| lot.received_to_date)
                .try_fold(rub(0), |sum, amount| sum.try_add(amount))
                .unwrap(),
            rub(101)
        );
    }

    #[test]
    fn a_coupon_before_a_second_purchase_stays_with_the_first_lot() {
        let trade = sample_trade();
        let first = Trade { units: 30, ..trade };
        let second = Trade {
            day: date!(2025 - 04 - 01),
            units: 70,
            gross: 700_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&first, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 03 - 15),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();
        book.apply(&buy(&second, 3), &rules).unwrap();

        let lots = book.entry(&key(&trade)).unwrap().lots();
        assert_eq!(lots[0].received_to_date, Some(rub(100)));
        assert_eq!(lots[1].received_to_date, None);
    }

    #[test]
    fn amortisation_records_the_returned_cash_on_each_lot() {
        let bond = Bond::new();
        let rules = RuleRegistry::with_defaults();
        let mut book = book_with_lots(
            &bond,
            vec![
                bond_lot(&bond, 30, known("1000", "1000"), 300_000),
                bond_lot(&bond, 70, known("1000", "1000"), 700_000),
            ],
        );

        book.apply(&bond.amortisation(100, "1", 101), &rules)
            .unwrap();

        let lots = book.entry(&bond.key()).unwrap().lots();
        assert_eq!(lots[0].received_to_date, Some(rub(30)));
        assert_eq!(lots[1].received_to_date, Some(rub(71)));
    }

    #[test]
    fn received_cash_is_split_when_a_lot_is_partially_sold() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();
        let partial = Trade {
            units: 40,
            day: date!(2025 - 06 - 01),
            gross: 500_000,
            ..trade
        };
        book.apply(&sell(&partial, 3), &rules).unwrap();

        assert_eq!(
            book.entry(&key(&trade)).unwrap().lots()[0].received_to_date,
            Some(rub(60))
        );
    }

    #[test]
    fn an_unpriced_quantity_receives_its_share_of_a_coupon_as_unallocated() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let priced = Trade {
            units: 50,
            gross: 500_000,
            ..trade
        };
        book.apply(&buy(&priced, 2), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                3,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots()[0].received_to_date, Some(rub(50)));
        assert_eq!(entry.unpriced_income(), Some(rub(50)));
        assert_eq!(entry.cohorts(), Err(CohortGap::AcquisitionDateUnknown));
    }

    #[test]
    fn an_income_without_an_approved_kind_refuses_cohorts() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: None,
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.lots()[0].received_to_date, None);
        assert!(entry.income_kind_unknown());
        assert_eq!(entry.cohorts(), Err(CohortGap::IncomeKindUnknown));
    }

    #[test]
    fn a_coupon_for_an_unknown_position_is_retained_as_a_marker() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                1,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Coupon),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert!(entry.lots().is_empty());
        assert_eq!(entry.unallocated_income(), Some(rub(100)));
        assert!(entry.cohorts().unwrap().is_empty());
    }

    #[test]
    fn cohorts_report_restored_without_basis_after_unpriced_quantity_is_sold() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        let restored = event_with(
            trade.account,
            date!(2024 - 01 - 01),
            1,
            EventKind::OpeningPosition {
                instrument: trade.instrument,
                quantity: qty(50),
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(
                trade.account,
                CustodyId::new_random(),
                trade.instrument,
                qty(50),
            )],
        );
        book.apply(&restored, &rules).unwrap();
        let sale = Trade {
            units: 50,
            day: date!(2025 - 02 - 01),
            gross: 500_000,
            ..trade
        };
        book.apply(&sell(&sale, 2), &rules).unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert_eq!(entry.unpriced(), qty(0));
        assert_eq!(entry.cohorts(), Err(CohortGap::RestoredWithoutBasis));
    }

    #[test]
    fn cohorts_group_same_acquisition_dates_and_keep_different_dates_separate() {
        let trade = sample_trade();
        let same_day_a = Trade {
            units: 30,
            gross: 300_000,
            ..trade
        };
        let same_day_b = Trade {
            units: 20,
            gross: 200_000,
            ..trade
        };
        let later = Trade {
            day: date!(2025 - 04 - 01),
            units: 50,
            gross: 500_000,
            ..trade
        };
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&dated_buy(&same_day_a, 1), &rules).unwrap();
        book.apply(&dated_buy(&same_day_b, 2), &rules).unwrap();
        book.apply(&dated_buy(&later, 3), &rules).unwrap();

        let cohorts = book.entry(&key(&trade)).unwrap().cohorts().unwrap();
        assert_eq!(cohorts.len(), 2);
        assert_eq!(cohorts[0].acquired, TradeDate(date!(2025 - 03 - 01)));
        assert_eq!(cohorts[0].quantity, qty(50));
        assert_eq!(cohorts[0].cost_basis, rub(520_000));
        assert_eq!(cohorts[0].acquisition_basis, Some(rub(520_000)));
        assert_eq!(cohorts[1].acquired, TradeDate(date!(2025 - 04 - 01)));
        assert_eq!(cohorts[1].quantity, qty(50));
        assert_eq!(cohorts[1].cost_basis, rub(510_000));
        assert_eq!(cohorts[1].acquisition_basis, Some(rub(510_000)));
    }

    #[test]
    fn a_dividend_does_not_block_cohort_construction() {
        let trade = sample_trade();
        let rules = RuleRegistry::with_defaults();
        let mut book = LotBook::new(LotRuleVersion(1));
        book.apply(&dated_buy(&trade, 1), &rules).unwrap();
        book.apply(
            &event_with(
                trade.account,
                date!(2025 - 04 - 01),
                2,
                EventKind::Income {
                    instrument: Some(trade.instrument),
                    gross: rub(100),
                    kind: Some(IncomeKind::Dividend),
                },
                vec![Leg::cash(trade.account, rub(100))],
            ),
            &rules,
        )
        .unwrap();

        let entry = book.entry(&key(&trade)).unwrap();
        assert!(!entry.income_kind_unknown());
        assert_eq!(entry.cohorts().unwrap().len(), 1);
    }

    #[test]
    fn cohorts_report_mixed_cost_basis_currencies() {
        let bond = Bond::new();
        let mut foreign = bond_lot(&bond, 10, known("1000", "1000"), 1_000_000);
        foreign.cost_basis = Money::new(PostedMinor::new(1_000_000), CurrencyCode::Usd);
        let book = book_with_lots(
            &bond,
            vec![
                bond_lot(&bond, 10, known("1000", "1000"), 1_000_000),
                foreign,
            ],
        );

        assert_eq!(
            book.entry(&bond.key()).unwrap().cohorts(),
            Err(CohortGap::InconsistentCostBasisCurrency)
        );
    }
    #[test]
    fn cohorts_refuse_missing_acquisition_date_and_cost_basis_overflow() {
        let bond = Bond::new();
        let mut missing_date = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        missing_date.acquired = None;
        assert_eq!(
            book_with_lots(&bond, vec![missing_date])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::AcquisitionDateUnknown)
        );

        let mut first = bond_lot(&bond, 1, known("1000", "1000"), i64::MAX);
        let mut second = bond_lot(&bond, 1, known("1000", "1000"), i64::MAX);
        first.acquisition_basis = None;
        second.acquisition_basis = None;
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentCostBasisOverflow)
        );
    }

    #[test]
    fn cohorts_refuse_quantity_and_optional_money_overflow_or_currency_mismatch() {
        let bond = Bond::new();
        let mut first = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        let mut second = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        first.quantity = Quantity(Dec::new(Decimal::MAX));
        second.quantity = Quantity(Dec::new(Decimal::MAX));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentQuantity)
        );

        let mut first = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        let mut second = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        first.accrued_interest_paid = Some(rub(1));
        second.accrued_interest_paid = Some(Money::new(PostedMinor::new(1), CurrencyCode::Usd));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentOptionalMoneyCurrency)
        );

        let mut first = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        let mut second = bond_lot(&bond, 1, known("1000", "1000"), 1_000);
        first.accrued_interest_paid = Some(rub(i64::MAX));
        second.accrued_interest_paid = Some(rub(i64::MAX));
        assert_eq!(
            book_with_lots(&bond, vec![first, second])
                .entry(&bond.key())
                .unwrap()
                .cohorts(),
            Err(CohortGap::InconsistentOptionalMoneyOverflow)
        );
    }
}
