//! Контрольные утверждения источника (§10.3).
//!
//! Отчёт брокера содержит не только операции, но и контрольные секции:
//! остатки на начало и конец периода, обороты Dt/Kt, количества бумаг,
//! суммы комиссий, купонов и дивидендов, удержанный налог. Это **факты
//! источника**, а не расчёт, поэтому они записываются в журнал наравне
//! с операциями — с provenance, версией парсера и локатором строки.
//!
//! Утверждение денег не двигает: ног у события нет, как у `Valuation`.
//! Нога здесь означала бы, что контрольная секция попала в остаток
//! вторым экземпляром.

use serde::{Deserialize, Serialize};
use time::Date;

use super::Dimension;
use crate::ids::{CustodyId, InstrumentId};
use crate::money::{CurrencyCode, PostedMinor, Quantity};

/// Интервал, о котором говорит утверждение. Границы включаются с обеих
/// сторон: отчёт за март говорит и о первом, и о тридцать первом марта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssertionPeriod {
    pub from: Date,
    pub to: Date,
}

impl AssertionPeriod {
    /// Интервал с началом позже конца не создаётся.
    ///
    /// Такой интервал — не «пустой период», а неверно разобранный
    /// документ: перепутанные местами даты дают сверку, которая никогда
    /// ни с чем не сойдётся и потому вечно висит расхождением.
    ///
    /// Проверка живёт не в `new`: `cargo-mutants` молча пропускает
    /// функции с этим именем (§15.7).
    #[must_use]
    pub fn between(from: Date, to: Date) -> Option<Self> {
        (from <= to).then_some(Self { from, to })
    }

    /// Корректен ли интервал.
    ///
    /// Нужен отдельно от конструктора: событие приходит и из JSON, где
    /// конструктор не вызывался, и валидация формы обязана проверять
    /// состояние, а не полагаться на то, что его кто-то собрал верно.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.from <= self.to
    }

    #[must_use]
    pub fn contains(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }
}

/// На какой момент интервала сделано утверждение об остатке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BalancePoint {
    /// Остаток на начало: состояние **до** первого события интервала.
    Opening,
    /// Остаток на конец: состояние, включающее последнее событие интервала.
    Closing,
}

impl BalancePoint {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Closing => "closing",
        }
    }
}

/// Что именно утверждает контрольная секция.
///
/// Величины оборотов и итогов — **модули**: знак несёт сторона
/// (дебет/кредит) и смысл поля, а не само число. Денежный остаток —
/// исключение: он может быть отрицательным, и это законное
/// состояние (§11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlClaim {
    /// Остаток денег на начало или конец интервала.
    CashBalance {
        currency: CurrencyCode,
        amount: PostedMinor,
        at: BalancePoint,
    },
    /// Количество бумаг на начало или конец интервала.
    PositionQuantity {
        instrument: InstrumentId,
        custody: CustodyId,
        quantity: Quantity,
        at: BalancePoint,
    },
    /// Обороты по счёту за интервал, обе стороны модулями.
    CashTurnover {
        currency: CurrencyCode,
        debit: PostedMinor,
        credit: PostedMinor,
    },
    /// Сумма комиссий за интервал.
    FeesTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Сумма купонов и дивидендов за интервал.
    IncomeTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
    /// Удержанный налоговым агентом налог за интервал.
    TaxWithheldTotal {
        currency: CurrencyCode,
        amount: PostedMinor,
    },
}

impl ControlClaim {
    /// Какое измерение ограничивает это утверждение (§10.3).
    ///
    /// Комиссии отнесены к деньгам, а не к доходам: комиссия — это
    /// денежное списание, и сходится она с денежной проекцией.
    /// Удержанный налог — единственное, что говорит о `TaxBasis`,
    /// и говорит он только об агрегате (основание 8).
    #[must_use]
    pub const fn dimension(&self) -> Dimension {
        match self {
            Self::CashBalance { .. } | Self::CashTurnover { .. } | Self::FeesTotal { .. } => {
                Dimension::Cash
            }
            Self::PositionQuantity { .. } => Dimension::Positions,
            Self::IncomeTotal { .. } => Dimension::Income,
            Self::TaxWithheldTotal { .. } => Dimension::TaxBasis,
        }
    }

    /// Машиночитаемое имя вида утверждения.
    #[must_use]
    pub const fn discriminant(&self) -> &'static str {
        match self {
            Self::CashBalance { .. } => "cash_balance",
            Self::PositionQuantity { .. } => "position_quantity",
            Self::CashTurnover { .. } => "cash_turnover",
            Self::FeesTotal { .. } => "fees_total",
            Self::IncomeTotal { .. } => "income_total",
            Self::TaxWithheldTotal { .. } => "tax_withheld_total",
        }
    }

    /// Величина, которая обязана быть неотрицательной, и имя её поля.
    ///
    /// `None` означает «отрицательное значение законно»: денежный
    /// остаток (§11) и количество, проверяемое отдельно как величина
    /// периметра, а не как знак итога.
    #[must_use]
    pub const fn non_negative_field(&self) -> Option<(&'static str, i64)> {
        match self {
            Self::CashBalance { .. } | Self::PositionQuantity { .. } => None,
            Self::CashTurnover { debit, credit, .. } => {
                // Проверяется меньшая из двух сторон: неотрицательная
                // меньшая означает неотрицательные обе. Взять первую
                // попавшуюся значило бы пропускать отрицательный кредит
                // при положительном дебете.
                let smaller = if debit.raw() <= credit.raw() {
                    debit.raw()
                } else {
                    credit.raw()
                };
                Some(("turnover", smaller))
            }
            Self::FeesTotal { amount, .. }
            | Self::IncomeTotal { amount, .. }
            | Self::TaxWithheldTotal { amount, .. } => Some(("amount", amount.raw())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(amount: i64) -> PostedMinor {
        PostedMinor::new(amount)
    }

    #[test]
    fn an_inverted_period_is_not_constructed() {
        assert!(AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).is_some());
        assert!(AssertionPeriod::between(date!(2026 - 03 - 31), date!(2026 - 03 - 01)).is_none());
    }

    #[test]
    fn a_single_day_period_is_valid() {
        // Отчёт за один день — законный документ, а не вырожденный случай.
        let day = date!(2026 - 03 - 15);
        let period = AssertionPeriod::between(day, day).unwrap();
        assert!(period.is_well_formed());
        assert!(period.contains(day));
    }

    #[test]
    fn period_boundaries_are_inclusive_on_both_ends() {
        let period =
            AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap();
        assert!(period.contains(date!(2026 - 03 - 01)));
        assert!(period.contains(date!(2026 - 03 - 31)));
        assert!(!period.contains(date!(2026 - 02 - 28)));
        assert!(!period.contains(date!(2026 - 04 - 01)));
    }

    #[test]
    fn a_period_built_around_the_constructor_is_recognised_as_malformed() {
        // Ровно тот случай, ради которого проверка живёт отдельно от
        // конструктора: структура собрана полями, минуя `between`.
        let inverted = AssertionPeriod {
            from: date!(2026 - 03 - 31),
            to: date!(2026 - 03 - 01),
        };
        assert!(!inverted.is_well_formed());
    }

    #[test]
    fn each_claim_constrains_exactly_one_dimension() {
        // Измерение выводится из вида утверждения, а не назначается
        // вызывающим: назначаемое измерение позволило бы объявить
        // сошедшийся остаток подтверждением налоговой базы.
        let cash = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(100),
            at: BalancePoint::Closing,
        };
        let fees = ControlClaim::FeesTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let position = ControlClaim::PositionQuantity {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: Quantity(Dec::new(Decimal::from(10))),
            at: BalancePoint::Closing,
        };
        let income = ControlClaim::IncomeTotal {
            currency: CurrencyCode::Rub,
            amount: rub(100),
        };
        let tax = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: rub(13),
        };

        assert_eq!(cash.dimension(), Dimension::Cash);
        assert_eq!(fees.dimension(), Dimension::Cash);
        assert_eq!(position.dimension(), Dimension::Positions);
        assert_eq!(income.dimension(), Dimension::Income);
        assert_eq!(tax.dimension(), Dimension::TaxBasis);
    }

    #[test]
    fn every_claim_kind_has_a_distinct_discriminant() {
        let claims = [
            ControlClaim::CashBalance {
                currency: CurrencyCode::Rub,
                amount: rub(1),
                at: BalancePoint::Opening,
            },
            ControlClaim::PositionQuantity {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: Quantity(Dec::one()),
                at: BalancePoint::Opening,
            },
            ControlClaim::CashTurnover {
                currency: CurrencyCode::Rub,
                debit: rub(1),
                credit: rub(1),
            },
            ControlClaim::FeesTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::IncomeTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
            ControlClaim::TaxWithheldTotal {
                currency: CurrencyCode::Rub,
                amount: rub(1),
            },
        ];
        let mut names: Vec<&str> = claims.iter().map(ControlClaim::discriminant).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "имена видов утверждений совпали");
    }

    #[test]
    fn a_turnover_reports_the_smaller_side_for_the_sign_check() {
        // Проверяется меньшая из двух сторон независимо от того, какая
        // именно отрицательна: иначе отрицательный кредит при
        // положительном дебете прошёл бы проверку.
        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(500),
            credit: rub(-1),
        };
        assert_eq!(claim.non_negative_field(), Some(("turnover", -1)));

        let mirrored = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: rub(-1),
            credit: rub(500),
        };
        assert_eq!(mirrored.non_negative_field(), Some(("turnover", -1)));
    }

    #[test]
    fn a_negative_cash_balance_is_not_a_sign_violation() {
        // §11: технический овердрафт и тайминги расчётов дают минус,
        // и он обязан войти в NAV обязательством, а не быть отвергнут.
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: rub(-5_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(claim.non_negative_field(), None);
    }
}
