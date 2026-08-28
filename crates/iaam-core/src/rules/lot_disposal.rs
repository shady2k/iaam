//! Списание лотов (§4.12).
//!
//! FIFO предписан ст. 214.1 НК РФ, но **не является глобальной очередью
//! по портфелю**: область задаётся налогоплательщиком, агентом, базой,
//! инструментом, счётом, режимом и годом. На этапе 1 область — пара
//! «счёт × инструмент»; расширение до полной области — эпик E5.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::dates::TradeDate;
use crate::ids::InstrumentId;
use crate::money::{Money, MoneyError, PerUnitAmount, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LotId(pub Uuid);

impl LotId {
    #[must_use]
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Экономический лот: партия приобретения.
///
/// Позиция является проекцией набора лотов, а не самостоятельной сущностью.
/// Без лотов невозможен ЛДВ: три года владения — свойство покупки,
/// у позиции со средней ценой возраста нет.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lot {
    pub id: LotId,
    pub instrument: InstrumentId,
    /// Может быть неизвестна для восстановленной позиции (§10.7).
    pub acquired: Option<TradeDate>,
    pub quantity: Quantity,
    pub cost_basis: Money,
    /// НКД, уплаченный при приобретении лота; неизвестное не превращается
    /// в ноль (§4.9).
    #[serde(default)]
    pub accrued_interest_paid: Option<Money>,
    /// Денежные выплаты, уже полученные этим лотом.
    #[serde(default)]
    pub received_to_date: Option<Money>,
    /// Непогашенный номинал — только у долговой бумаги; у акции его нет
    /// и не бывает, поэтому умолчание `Unknown` здесь честно.
    ///
    /// `#[serde(default)]` обязателен: снимки проекций и архивы записаны
    /// до E3.4 и этого поля не содержат. Без умолчания старый архив
    /// перестал бы открываться.
    #[serde(default)]
    pub principal: PrincipalState,
}

/// Состояние непогашенного номинала лота (§6.5).
///
/// Величины — **на одну бумагу**: непогашенный номинал лота равен
/// `quantity × remaining_per_unit`, поэтому частичное списание лота
/// ничего не пересчитывает. При размерности «на лот» каждое списание
/// требовало бы пересчёта, а значит и повода ошибиться.
///
/// Один тип вместо двух независимых `Option`: пара полей допускала бы
/// «номинал неизвестен, остаток известен», две валюты и остаток больше
/// первоначального — состояния, которых не бывает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrincipalState {
    /// Номинал неизвестен: бумага заведена до того, как справочник его
    /// узнал, — или это вовсе не долговая бумага. Подставлять ноль
    /// запрещено (§4.9).
    #[default]
    Unknown,
    Known {
        original_per_unit: PerUnitAmount,
        remaining_per_unit: PerUnitAmount,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PrincipalError {
    #[error("непогашенный остаток номинала больше первоначального")]
    RemainingAboveOriginal,
    #[error("номинал и остаток номинала в разных валютах")]
    CurrencyMismatch,
    #[error("номинал не может быть отрицательным")]
    Negative,
    #[error("номинал неизвестен: уменьшать нечего")]
    Unknown,
}

impl PrincipalState {
    /// Известный номинал с проверкой инвариантов.
    ///
    /// Конструктор, а не публичные поля варианта: собранное вручную
    /// `Known` обошло бы все три проверки.
    pub fn known(
        original_per_unit: PerUnitAmount,
        remaining_per_unit: PerUnitAmount,
    ) -> Result<Self, PrincipalError> {
        if original_per_unit.currency() != remaining_per_unit.currency() {
            return Err(PrincipalError::CurrencyMismatch);
        }
        if original_per_unit.value().is_negative() || remaining_per_unit.value().is_negative() {
            return Err(PrincipalError::Negative);
        }
        if remaining_per_unit.value() > original_per_unit.value() {
            return Err(PrincipalError::RemainingAboveOriginal);
        }
        Ok(Self::Known {
            original_per_unit,
            remaining_per_unit,
        })
    }

    /// Остаток после возврата части номинала. Первоначальный номинал
    /// не меняется: доля возврата считается от него в E5.
    ///
    /// Неизвестный номинал отказывает, а не остаётся собой: принять факт
    /// амортизации и молча его не применить — ровно то, чего этот план
    /// не допускает.
    pub fn reduced_by(self, returned_per_unit: PerUnitAmount) -> Result<Self, PrincipalError> {
        let Self::Known {
            original_per_unit,
            remaining_per_unit,
        } = self
        else {
            return Err(PrincipalError::Unknown);
        };
        if remaining_per_unit.currency() != returned_per_unit.currency() {
            return Err(PrincipalError::CurrencyMismatch);
        }
        let left = remaining_per_unit
            .value()
            .checked_sub(returned_per_unit.value())
            .map_err(|_| PrincipalError::Negative)?;
        Self::known(
            original_per_unit,
            PerUnitAmount::new(left, remaining_per_unit.currency()),
        )
    }

    /// Остаток на одну бумагу, если он известен.
    #[must_use]
    pub const fn remaining_per_unit(&self) -> Option<PerUnitAmount> {
        match self {
            Self::Unknown => None,
            Self::Known {
                remaining_per_unit, ..
            } => Some(*remaining_per_unit),
        }
    }
}

/// Идентификатор версии правила. Входит в результат и в след аудита.
///
/// Владеющая `String`, а не `&'static str`: десериализация заимствованной
/// строки с временем жизни `'static` из обычного JSON не является корректным
/// контрактом — входные данные столько не живут.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RuleId(pub String);

impl RuleId {
    /// Тривиальная упаковка поля: логики, которую стоило бы вынести ради
    /// мутационного заслона, здесь нет, поэтому слепота `cargo-mutants`
    /// к имени `new` ничего не скрывает.
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisposalInput {
    /// Лоты в порядке приобретения. Порядок обеспечивает вызывающий.
    pub lots: Vec<Lot>,
    pub quantity: Quantity,
}

/// Часть лота, списанная при выбытии.
#[derive(Debug, Clone, PartialEq)]
pub struct DisposedPart {
    pub lot: LotId,
    pub quantity: Quantity,
    pub basis_released: Money,
    pub acquired: Option<TradeDate>,
    pub accrued_interest_released: Option<Money>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisposalResult {
    pub rule: RuleId,
    pub disposed: Vec<DisposedPart>,
    pub remaining: Vec<Lot>,
    /// Суммарная списанная стоимость. Компонент тождества §6.5.
    pub basis_released: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DisposalError {
    #[error("недостаточно количества: запрошено {requested}, доступно {available}")]
    InsufficientQuantity {
        requested: String,
        available: String,
    },
    #[error("лоты в разных валютах не поддерживаются в одном выбытии")]
    MixedCurrencies,
    #[error("список лотов пуст")]
    NoLots,
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// Стратегия списания. Доменная стратегия, **не порт ввода-вывода**:
/// передаётся в ядро как неизменяемый вход, чистота сохраняется (§3.2).
pub trait LotDisposalRule: Send + Sync {
    fn id(&self) -> RuleId;
    fn apply(&self, input: &DisposalInput) -> Result<DisposalResult, DisposalError>;
}

/// FIFO по ст. 214.1 НК РФ.
///
/// Specific lot identification в РФ недоступна: продал — списались первые
/// по времени приобретения, независимо от намерения.
#[derive(Debug, Clone, Copy, Default)]
pub struct FifoV1;

impl FifoV1 {
    pub const ID: &'static str = "fifo/214.1/v1";
}

impl LotDisposalRule for FifoV1 {
    fn id(&self) -> RuleId {
        RuleId::new(Self::ID)
    }

    fn apply(&self, input: &DisposalInput) -> Result<DisposalResult, DisposalError> {
        let Some(first) = input.lots.first() else {
            return Err(DisposalError::NoLots);
        };
        let currency = first.cost_basis.currency();
        if input
            .lots
            .iter()
            .any(|l| l.cost_basis.currency() != currency)
        {
            return Err(DisposalError::MixedCurrencies);
        }

        let available: Decimal = input.lots.iter().map(|l| l.quantity.0.inner()).sum();
        let requested = input.quantity.0.inner();
        if requested > available {
            return Err(DisposalError::InsufficientQuantity {
                requested: requested.to_string(),
                available: available.to_string(),
            });
        }

        let mut left = requested;
        let mut disposed = Vec::new();
        let mut remaining = Vec::new();

        for lot in &input.lots {
            let lot_qty = lot.quantity.0.inner();
            if left.is_zero() {
                remaining.push(lot.clone());
                continue;
            }
            if lot_qty <= left {
                // Лот списывается целиком.
                disposed.push(DisposedPart {
                    lot: lot.id,
                    quantity: lot.quantity,
                    basis_released: lot.cost_basis,
                    acquired: lot.acquired,
                    accrued_interest_released: lot.accrued_interest_paid,
                });
                left -= lot_qty;
            } else {
                // Лот делится. Денежные величины разносится пропорционально
                // количеству; остаток округления остаётся у невыбывшей части.
                let taken_basis = split_basis(lot.cost_basis, left, lot_qty)?;
                let kept_basis = lot.cost_basis.try_sub(taken_basis)?;
                let (taken_accrued_interest, kept_accrued_interest) =
                    split_optional_basis(lot.accrued_interest_paid, left, lot_qty)?;
                let (_, kept_received) =
                    split_optional_basis(lot.received_to_date, left, lot_qty)?;
                disposed.push(DisposedPart {
                    lot: lot.id,
                    quantity: Quantity(Dec::new(left)),
                    basis_released: taken_basis,
                    acquired: lot.acquired,
                    accrued_interest_released: taken_accrued_interest,
                });
                remaining.push(Lot {
                    quantity: Quantity(Dec::new(lot_qty - left)),
                    cost_basis: kept_basis,
                    accrued_interest_paid: kept_accrued_interest,
                    received_to_date: kept_received,
                    ..lot.clone()
                });
                left = Decimal::ZERO;
            }
        }

        let released: Vec<Money> = disposed.iter().map(|d| d.basis_released).collect();
        let basis_released = Money::sum(&released, currency)?;

        Ok(DisposalResult {
            rule: self.id(),
            disposed,
            remaining,
            basis_released,
        })
    }
}

/// Разнесение стоимости лота пропорционально списываемому количеству.
///
/// Округление — половина к чётному, однократно, на границе представления
/// в минимальных единицах (§6.6). Остаток от округления остаётся
/// в невыбывшей части: суммарная стоимость лота сохраняется.
pub(crate) fn split_basis(
    total: Money,
    taken_qty: Decimal,
    lot_qty: Decimal,
) -> Result<Money, DisposalError> {
    debug_assert!(!lot_qty.is_zero(), "количество лота не может быть нулевым");
    let minor = Decimal::from(total.amount().raw());
    let scaled = (minor * taken_qty) / lot_qty;
    let rounded =
        scaled.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven);
    let value = i64::try_from(rounded.trunc().mantissa())
        .map_err(|_| DisposalError::Money(MoneyError::Overflow))?;
    Ok(Money::new(PostedMinor::new(value), total.currency()))
}

fn split_optional_basis(
    total: Option<Money>,
    taken_qty: Decimal,
    lot_qty: Decimal,
) -> Result<(Option<Money>, Option<Money>), DisposalError> {
    let Some(total) = total else {
        return Ok((None, None));
    };
    let taken = split_basis(total, taken_qty, lot_qty)?;
    let kept = total.try_sub(taken)?;
    Ok((Some(taken), Some(kept)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::{CurrencyCode, PostedMinor};
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Rub,
        )
    }

    fn usd_per_unit(text: &str) -> PerUnitAmount {
        PerUnitAmount::new(
            Dec::new(Decimal::from_str_exact(text).unwrap()),
            CurrencyCode::Usd,
        )
    }

    // --- Непогашенный номинал ---

    #[test]
    fn a_remaining_principal_above_the_original_is_refused() {
        assert_eq!(
            PrincipalState::known(per_unit("1000"), per_unit("1200")).unwrap_err(),
            PrincipalError::RemainingAboveOriginal
        );
    }

    #[test]
    fn principal_in_two_currencies_is_refused() {
        assert_eq!(
            PrincipalState::known(per_unit("1000"), usd_per_unit("500")).unwrap_err(),
            PrincipalError::CurrencyMismatch
        );
    }

    #[test]
    fn a_negative_principal_is_refused_on_either_side() {
        assert_eq!(
            PrincipalState::known(per_unit("-1000"), per_unit("-1000")).unwrap_err(),
            PrincipalError::Negative
        );
        assert_eq!(
            PrincipalState::known(per_unit("1000"), per_unit("-1")).unwrap_err(),
            PrincipalError::Negative
        );
    }

    #[test]
    fn a_fully_amortised_principal_is_a_valid_state() {
        // Ноль остатка — не ошибка: бумага погашена, и это факт.
        let state = PrincipalState::known(per_unit("1000"), per_unit("0")).unwrap();
        assert_eq!(
            state,
            PrincipalState::Known {
                original_per_unit: per_unit("1000"),
                remaining_per_unit: per_unit("0"),
            }
        );
    }

    #[test]
    fn an_amortisation_lowers_the_remaining_principal_and_keeps_the_original() {
        let state = PrincipalState::known(per_unit("1000"), per_unit("1000")).unwrap();
        assert_eq!(
            state.reduced_by(per_unit("200")).unwrap(),
            PrincipalState::Known {
                original_per_unit: per_unit("1000"),
                remaining_per_unit: per_unit("800"),
            }
        );
    }

    #[test]
    fn an_amortisation_larger_than_the_remaining_principal_is_refused() {
        let state = PrincipalState::known(per_unit("1000"), per_unit("200")).unwrap();
        assert_eq!(
            state.reduced_by(per_unit("201")).unwrap_err(),
            PrincipalError::Negative
        );
    }

    #[test]
    fn an_amortisation_in_another_currency_is_refused() {
        let state = PrincipalState::known(per_unit("1000"), per_unit("1000")).unwrap();
        assert_eq!(
            state.reduced_by(usd_per_unit("200")).unwrap_err(),
            PrincipalError::CurrencyMismatch
        );
    }

    #[test]
    fn an_unknown_principal_cannot_be_amortised_silently() {
        // Пропустить уменьшение неизвестного номинала значило бы принять
        // факт и не применить его — ровно то, что запрещено (§4.9).
        assert_eq!(
            PrincipalState::Unknown
                .reduced_by(per_unit("200"))
                .unwrap_err(),
            PrincipalError::Unknown
        );
    }

    #[test]
    fn a_lot_written_before_principal_existed_reads_as_unknown() {
        // Снимки проекций и архивы записаны до E3.4 и этого поля
        // не содержат: без serde(default) старый архив не открылся бы.
        let value = serde_json::json!({
            "id": LotId::new_random(),
            "instrument": InstrumentId::new_random(),
            "acquired": null,
            "quantity": qty(10),
            "cost_basis": rub(100_000),
        });
        let lot: Lot = serde_json::from_value(value).unwrap();
        assert_eq!(lot.principal, PrincipalState::Unknown);
        assert_eq!(lot.accrued_interest_paid, None);
        assert_eq!(lot.received_to_date, None);
    }

    fn qty(n: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(n)))
    }

    /// Два лота: сначала дороже, потом дешевле.
    /// Ровно случай из постановки задачи: «купил 10 яблок дороже,
    /// потом 10 дешевле, брокер показывает среднюю».
    fn two_lots() -> Vec<Lot> {
        vec![
            Lot {
                id: LotId::new_random(),
                instrument: InstrumentId::new_random(),
                acquired: Some(TradeDate(date!(2026 - 01 - 10))),
                accrued_interest_paid: None,
                received_to_date: None,
                quantity: qty(10),
                cost_basis: rub(100_000), // 10 шт по 100 ₽
                principal: PrincipalState::Unknown,
            },
            Lot {
                id: LotId::new_random(),
                instrument: InstrumentId::new_random(),
                acquired: Some(TradeDate(date!(2026 - 02 - 10))),
                accrued_interest_paid: None,
                received_to_date: None,
                quantity: qty(10),
                cost_basis: rub(90_000), // 10 шт по 90 ₽
                principal: PrincipalState::Unknown,
            },
        ]
    }

    /// Один лот с заданными количеством и стоимостью — для проверки
    /// разнесения стоимости при делении лота.
    fn single_lot(quantity: i64, basis_minor: i64) -> Vec<Lot> {
        vec![Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            accrued_interest_paid: None,
            received_to_date: None,
            acquired: Some(TradeDate(date!(2026 - 03 - 10))),
            quantity: qty(quantity),
            cost_basis: rub(basis_minor),
            principal: PrincipalState::Unknown,
        }]
    }

    #[test]
    fn selling_ten_takes_the_first_lot_whole_not_the_average() {
        let lots = two_lots();
        let rule = FifoV1;
        let out = rule
            .apply(&DisposalInput {
                lots: lots.clone(),
                quantity: qty(10),
            })
            .unwrap();

        assert_eq!(out.disposed.len(), 1, "списан ровно один лот");
        assert_eq!(out.disposed[0].lot, lots[0].id, "списан первый по времени");
        assert_eq!(
            out.basis_released,
            rub(100_000),
            "по цене первого лота, не средней"
        );
        assert_eq!(out.remaining.len(), 1);
        assert_eq!(out.remaining[0].quantity, qty(10));
    }

    #[test]
    fn selling_fifteen_splits_the_second_lot() {
        let lots = two_lots();
        let out = FifoV1
            .apply(&DisposalInput {
                lots: lots.clone(),
                quantity: qty(15),
            })
            .unwrap();

        assert_eq!(out.disposed.len(), 2);
        // 1000,00 за первый лот целиком + половина второго = 450,00
        assert_eq!(out.basis_released, rub(145_000));
        assert_eq!(out.disposed[1].lot, lots[1].id);
        assert_eq!(out.disposed[1].quantity, qty(5));
        assert_eq!(out.disposed[1].basis_released, rub(45_000));
        assert_eq!(out.disposed[1].acquired, lots[1].acquired);
        assert_eq!(out.remaining.len(), 1);
        assert_eq!(out.remaining[0].quantity, qty(5));
        assert_eq!(out.remaining[0].cost_basis, rub(45_000));
    }

    #[test]
    fn selling_more_than_held_is_an_error() {
        let out = FifoV1.apply(&DisposalInput {
            lots: two_lots(),
            quantity: qty(25),
        });
        assert!(matches!(
            out,
            Err(DisposalError::InsufficientQuantity { .. })
        ));
    }

    #[test]
    fn result_records_which_rule_was_applied() {
        let out = FifoV1
            .apply(&DisposalInput {
                lots: two_lots(),
                quantity: qty(1),
            })
            .unwrap();
        assert_eq!(out.rule, RuleId::new(FifoV1::ID));
    }

    #[test]
    fn selling_everything_leaves_nothing() {
        let out = FifoV1
            .apply(&DisposalInput {
                lots: two_lots(),
                quantity: qty(20),
            })
            .unwrap();
        assert!(out.remaining.is_empty());
        assert_eq!(out.basis_released, rub(190_000));
    }

    #[test]
    fn selling_nothing_disposes_nothing_and_keeps_every_lot() {
        let out = FifoV1
            .apply(&DisposalInput {
                lots: two_lots(),
                quantity: qty(0),
            })
            .unwrap();
        assert!(out.disposed.is_empty());
        assert_eq!(out.remaining.len(), 2);
        assert_eq!(out.basis_released, rub(0));
    }

    #[test]
    fn an_empty_lot_list_is_an_error_not_a_zero_basis() {
        // §4.9: неизвестное — не нулевая заглушка. Списывать не из чего —
        // это отказ, а не выбытие на нулевую стоимость.
        let out = FifoV1.apply(&DisposalInput {
            lots: Vec::new(),
            quantity: qty(1),
        });
        assert!(matches!(out, Err(DisposalError::NoLots)));
    }

    #[test]
    fn lots_in_different_currencies_are_rejected() {
        let mut lots = two_lots();
        lots[1].cost_basis = Money::new(PostedMinor::new(90_000), CurrencyCode::Usd);
        let out = FifoV1.apply(&DisposalInput {
            lots,
            quantity: qty(15),
        });
        assert!(matches!(out, Err(DisposalError::MixedCurrencies)));
    }

    #[test]
    fn splitting_rounds_halves_to_the_even_minor_unit() {
        // 5 минимальных единиц на 2 штуки: половина — 2,5, к чётному → 2.
        let down = FifoV1
            .apply(&DisposalInput {
                lots: single_lot(2, 5),
                quantity: qty(1),
            })
            .unwrap();
        assert_eq!(down.basis_released, rub(2), "2,5 → 2, а не 3");

        // 15 минимальных единиц на 2 штуки: половина — 7,5, к чётному → 8.
        let up = FifoV1
            .apply(&DisposalInput {
                lots: single_lot(2, 15),
                quantity: qty(1),
            })
            .unwrap();
        assert_eq!(up.basis_released, rub(8), "7,5 → 8, а не 7");
    }

    #[test]
    fn the_rounding_remainder_stays_with_the_part_not_disposed() {
        // Суммарная стоимость лота сохраняется: округление не создаёт
        // и не уничтожает копейки.
        let out = FifoV1
            .apply(&DisposalInput {
                lots: single_lot(3, 100),
                quantity: qty(1),
            })
            .unwrap();
        assert_eq!(out.basis_released, rub(33), "100/3 = 33,33… → 33");
        assert_eq!(out.remaining[0].cost_basis, rub(67));
        assert_eq!(out.remaining[0].quantity, qty(2));
    }

    #[test]
    fn a_lot_id_is_not_the_nil_uuid() {
        assert_ne!(LotId::new_random(), LotId::new_random());
    }

    #[test]
    fn the_rule_reports_its_own_identifier() {
        assert_eq!(FifoV1.id(), RuleId::new("fifo/214.1/v1"));
    }
    #[test]
    fn splitting_a_lot_splits_accrued_and_received_values_and_full_disposal_keeps_them() {
        let lot = Lot {
            id: LotId::new_random(),
            instrument: InstrumentId::new_random(),
            acquired: Some(TradeDate(date!(2026 - 03 - 10))),
            quantity: qty(100),
            cost_basis: rub(10_000),
            accrued_interest_paid: Some(rub(1_000)),
            received_to_date: Some(rub(800)),
            principal: PrincipalState::Unknown,
        };

        let partial = FifoV1
            .apply(&DisposalInput {
                lots: vec![lot.clone()],
                quantity: qty(50),
            })
            .unwrap();

        assert_eq!(partial.disposed[0].basis_released, rub(5_000));
        assert_eq!(partial.disposed[0].accrued_interest_released, Some(rub(500)));
        assert_eq!(partial.remaining[0].cost_basis, rub(5_000));
        assert_eq!(partial.remaining[0].accrued_interest_paid, Some(rub(500)));
        assert_eq!(partial.remaining[0].received_to_date, Some(rub(400)));

        let full = FifoV1
            .apply(&DisposalInput {
                lots: vec![lot.clone()],
                quantity: qty(100),
            })
            .unwrap();

        assert_eq!(full.disposed[0].basis_released, lot.cost_basis);
        assert_eq!(
            full.disposed[0].accrued_interest_released,
            lot.accrued_interest_paid
        );
        assert!(full.remaining.is_empty());
    }
}
