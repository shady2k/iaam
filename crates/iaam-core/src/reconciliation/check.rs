//! Сопоставление утверждения источника с наблюдаемым (§10.3, §10.4).
//!
//! **Допуска нет.** Обе стороны — проведённые суммы в минимальных
//! единицах валюты, и различие в копейку является различием. Порог
//! невязки существует там, где сравниваются расчётная величина и
//! проведённая (начисления по вкладу, §8.3), — это E3, и порог там
//! берётся из алгоритма округления договора, а не назначается здесь.

use super::claim::ControlClaim;
use super::observed::{ObservedTotals, Turnover};
use crate::money::{CurrencyCode, PostedMinor, Quantity};
use crate::numeric::decimal::Dec;

/// Величина одной стороны сравнения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimValue {
    Money {
        amount: PostedMinor,
        currency: CurrencyCode,
    },
    Quantity(Quantity),
}

/// Расхождение: что заявлено, что наблюдается, какова разница.
///
/// Разница считается как заявленное минус наблюдаемое: положительная
/// означает «источник видит больше, чем мы».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discrepancy {
    /// Поле утверждения, которое не сошлось. Для оборотов называет
    /// сторону: `debit` или `credit`.
    pub field: &'static str,
    pub claimed: ClaimValue,
    pub observed: ClaimValue,
    pub delta: ClaimValue,
}

/// Почему сравнение невозможно.
///
/// Невозможность сравнить — **не** расхождение. Расхождение означает
/// «цифры разошлись, разберитесь»; невозможность означает «сверять
/// не с чем», и это разные ответы владельцу (§10.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotComparable {
    /// У счёта нет ни одного события: подтверждать нечего.
    NoJournalCoverage,
    /// Налоговых фактов система пока не записывает (E5).
    TaxFactsNotRecorded,
}

impl NotComparable {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoJournalCoverage => "no_journal_coverage",
            Self::TaxFactsNotRecorded => "tax_facts_not_recorded",
        }
    }
}

/// Расхождение, объяснённое границей периметра (§11).
///
/// Существует, чтобы владелец не получал задание «починить» то, что
/// система намеренно не поддерживает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationException {
    /// Количества разошлись из-за обременения бумаг по РЕПО.
    UnsupportedRepoEncumbrance,
    /// В периоде присутствует финансирование вне периметра (маржа).
    UnsupportedFinancingPresent,
}

impl ReconciliationException {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedRepoEncumbrance => "unsupported_repo_encumbrance",
            Self::UnsupportedFinancingPresent => "unsupported_financing_present",
        }
    }
}

/// Итог сверки одного утверждения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Matched,
    Discrepant(Discrepancy),
    NotComparable {
        reason: NotComparable,
    },
    /// Расхождение объяснено границей периметра и не требует действий
    /// владельца (§11). Основанием повышения статуса не является.
    Excepted {
        exception: ReconciliationException,
    },
}

impl ClaimOutcome {
    /// Даёт ли исход право повысить статус измерения.
    ///
    /// Исключение периметра не даёт: «мы знаем, почему не сходится» —
    /// это не «сошлось».
    #[must_use]
    pub const fn confirms(&self) -> bool {
        match self {
            Self::Matched => true,
            Self::Discrepant(_) | Self::NotComparable { .. } | Self::Excepted { .. } => false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Discrepant(_) => "discrepant",
            Self::NotComparable { .. } => "not_comparable",
            Self::Excepted { .. } => "excepted",
        }
    }
}

/// Сверка одного утверждения с наблюдаемыми величинами.
#[must_use]
pub fn check_claim(claim: &ControlClaim, observed: &ObservedTotals) -> ClaimOutcome {
    if observed.events_seen() == 0 {
        return ClaimOutcome::NotComparable {
            reason: NotComparable::NoJournalCoverage,
        };
    }
    match *claim {
        ControlClaim::CashBalance {
            currency,
            amount,
            at,
        } => compare_money(
            "amount",
            currency,
            amount,
            observed
                .cash_at(at, currency)
                .unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at,
        } => compare_quantity(
            quantity,
            observed
                .position_at(at, instrument, custody)
                .unwrap_or_else(Quantity::zero),
        ),
        ControlClaim::CashTurnover {
            currency,
            debit,
            credit,
        } => {
            let Turnover {
                debit: seen_debit,
                credit: seen_credit,
            } = observed.turnover(currency).unwrap_or_default();
            match compare_money("debit", currency, debit, seen_debit) {
                ClaimOutcome::Matched => compare_money("credit", currency, credit, seen_credit),
                other => other,
            }
        }
        ControlClaim::FeesTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.fees(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::IncomeTotal { currency, amount } => compare_money(
            "amount",
            currency,
            amount,
            observed.income(currency).unwrap_or(PostedMinor::new(0)),
        ),
        ControlClaim::TaxWithheldTotal { currency, amount } => {
            if observed.tax_facts_recorded() {
                compare_money(
                    "amount",
                    currency,
                    amount,
                    observed
                        .tax_withheld(currency)
                        .unwrap_or(PostedMinor::new(0)),
                )
            } else {
                ClaimOutcome::NotComparable {
                    reason: NotComparable::TaxFactsNotRecorded,
                }
            }
        }
    }
}

/// Сравнение проведённых сумм. Точное: допуска нет.
fn compare_money(
    field: &'static str,
    currency: CurrencyCode,
    claimed: PostedMinor,
    observed: PostedMinor,
) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // Переполнение разницы означает величины, между которыми разрыв
    // больше диапазона денежного типа: это расхождение в любом случае,
    // и сообщается оно насыщением, а не паникой.
    let delta = claimed.raw().saturating_sub(observed.raw());
    ClaimOutcome::Discrepant(Discrepancy {
        field,
        claimed: ClaimValue::Money {
            amount: claimed,
            currency,
        },
        observed: ClaimValue::Money {
            amount: observed,
            currency,
        },
        delta: ClaimValue::Money {
            amount: PostedMinor::new(delta),
            currency,
        },
    })
}

fn compare_quantity(claimed: Quantity, observed: Quantity) -> ClaimOutcome {
    if claimed == observed {
        return ClaimOutcome::Matched;
    }
    // Невычислимая разница всё равно является расхождением: стороны
    // уже названы, и сообщать о ней отказом значило бы потерять сам
    // факт несовпадения.
    let delta = claimed
        .0
        .checked_sub(observed.0)
        .unwrap_or_else(|_| Dec::zero());
    ClaimOutcome::Discrepant(Discrepancy {
        field: "quantity",
        claimed: ClaimValue::Quantity(claimed),
        observed: ClaimValue::Quantity(observed),
        delta: ClaimValue::Quantity(Quantity(delta)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::Money;
    use crate::reconciliation::claim::{AssertionPeriod, BalancePoint};
    use crate::reconciliation::observed::observe;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn march() -> AssertionPeriod {
        AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31)).unwrap()
    }

    fn journal_with_one_deposit(account: AccountId, minor: i64) -> Vec<crate::event::Event> {
        vec![event_with(
            account,
            date!(2026 - 03 - 10),
            1,
            EventKind::CashIn { amount: rub(minor) },
            vec![Leg::cash(account, rub(minor))],
        )]
    }

    #[test]
    fn an_exact_match_is_accepted_and_one_kopeck_is_not() {
        // Допуска нет. Обе стороны — проведённые суммы в минимальных
        // единицах; «почти сошлось» на копейку означает потерянную
        // копейку, а потерянная копейка — это ошибка разнесения,
        // которая на длинной истории вырастает.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let exact = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&exact, &observed), ClaimOutcome::Matched);

        let off_by_one = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_001),
            at: BalancePoint::Closing,
        };
        let outcome = check_claim(&off_by_one, &observed);
        let ClaimOutcome::Discrepant(discrepancy) = outcome else {
            panic!("расхождение в одну копейку обязано быть расхождением: {outcome:?}");
        };
        assert_eq!(discrepancy.field, "amount");
        assert_eq!(
            discrepancy.delta,
            ClaimValue::Money {
                amount: PostedMinor::new(1),
                currency: CurrencyCode::Rub
            },
            "разница считается как заявленное минус наблюдаемое"
        );
    }

    #[test]
    fn an_empty_journal_is_not_comparable_rather_than_wrong() {
        // Утверждение «на счёте 100 000» при пустом журнале не является
        // расхождением на 100 000: сверять не с чем. Расхождение здесь
        // отправило бы владельца искать ошибку там, где её нет,
        // а нужен ему вердикт needs_reconciliation.
        let account = AccountId::new_random();
        let observed = observe(&[], account, march()).unwrap();
        let claim = ControlClaim::CashBalance {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(100_000),
            at: BalancePoint::Closing,
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::NoJournalCoverage
            }
        );
    }

    #[test]
    fn a_currency_without_movement_is_compared_as_zero_when_history_exists() {
        // История счёта есть, движения в долларах — нет. Утверждение
        // «на счёте 0 USD» подтверждается, а «на счёте 500 USD» —
        // расходится. Отдать здесь NotComparable значило бы навсегда
        // оставить непроверяемой любую валюту, в которой ничего не было.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let zero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(0),
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&zero, &observed), ClaimOutcome::Matched);

        let nonzero = ControlClaim::CashBalance {
            currency: CurrencyCode::Usd,
            amount: PostedMinor::new(50_000),
            at: BalancePoint::Closing,
        };
        assert!(matches!(
            check_claim(&nonzero, &observed),
            ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn a_turnover_names_the_side_that_disagrees() {
        // «Обороты не сошлись» без указания стороны заставляет владельца
        // сверять обе колонки вручную — ровно та работа, которую §10.2
        // отказывается на него перекладывать.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();

        let claim = ControlClaim::CashTurnover {
            currency: CurrencyCode::Rub,
            debit: PostedMinor::new(100_000),
            credit: PostedMinor::new(700),
        };
        let ClaimOutcome::Discrepant(discrepancy) = check_claim(&claim, &observed) else {
            panic!("расход 700 против нуля обязан быть расхождением");
        };
        assert_eq!(discrepancy.field, "credit");
    }

    #[test]
    fn tax_without_tax_facts_is_not_comparable() {
        // Налоговых фактов не производит ни один путь записи до E5.
        // Ноль с нашей стороны означает «не считаем», и объявить
        // удержанные брокером 1 300 расхождением было бы ложью.
        let account = AccountId::new_random();
        let observed = observe(
            &journal_with_one_deposit(account, 100_000),
            account,
            march(),
        )
        .unwrap();
        let claim = ControlClaim::TaxWithheldTotal {
            currency: CurrencyCode::Rub,
            amount: PostedMinor::new(130_000),
        };
        assert_eq!(
            check_claim(&claim, &observed),
            ClaimOutcome::NotComparable {
                reason: NotComparable::TaxFactsNotRecorded
            }
        );
    }

    #[test]
    fn a_position_quantity_is_compared_per_custody() {
        // То же количество в другом депозитарии — это другая позиция:
        // перевод бумаг между депозитариями внутри одного брокера
        // является реальной операцией (§4.5).
        let account = AccountId::new_random();
        let custody = CustodyId::new_random();
        let instrument = InstrumentId::new_random();
        let quantity = Quantity(Dec::new(Decimal::from(10)));
        let events = vec![event_with(
            account,
            date!(2026 - 03 - 11),
            1,
            EventKind::OpeningPosition {
                instrument,
                quantity,
                cost_basis: None,
                assertions: crate::event::kind::OpeningAssertions::default(),
            },
            vec![Leg::security(account, custody, instrument, quantity)],
        )];
        let observed = observe(&events, account, march()).unwrap();

        let matching = ControlClaim::PositionQuantity {
            instrument,
            custody,
            quantity,
            at: BalancePoint::Closing,
        };
        assert_eq!(check_claim(&matching, &observed), ClaimOutcome::Matched);

        let elsewhere = ControlClaim::PositionQuantity {
            instrument,
            custody: CustodyId::new_random(),
            quantity,
            at: BalancePoint::Closing,
        };
        assert!(matches!(
            check_claim(&elsewhere, &observed),
            ClaimOutcome::Discrepant(_)
        ));
    }

    #[test]
    fn each_reason_for_incomparability_has_a_distinct_code() {
        // «Нечего сверять» и «налоговых фактов нет» — разные ответы
        // владельцу: первый требует назвать остаток, второй не требует
        // ничего до E5. Один код на оба сделал бы их неразличимыми.
        assert_eq!(
            NotComparable::NoJournalCoverage.code(),
            "no_journal_coverage"
        );
        assert_eq!(
            NotComparable::TaxFactsNotRecorded.code(),
            "tax_facts_not_recorded"
        );
    }

    #[test]
    fn every_outcome_has_a_distinct_code() {
        let outcomes = [
            ClaimOutcome::Matched,
            ClaimOutcome::NotComparable {
                reason: NotComparable::NoJournalCoverage,
            },
            ClaimOutcome::Excepted {
                exception: ReconciliationException::UnsupportedRepoEncumbrance,
            },
        ];
        let mut codes: Vec<&str> = outcomes.iter().map(ClaimOutcome::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
        assert_eq!(
            ReconciliationException::UnsupportedFinancingPresent.code(),
            "unsupported_financing_present"
        );
    }

    #[test]
    fn an_exception_neither_confirms_nor_is_a_discrepancy() {
        // §11: «мы знаем, почему не сходится» — это не «сошлось».
        let excepted = ClaimOutcome::Excepted {
            exception: ReconciliationException::UnsupportedRepoEncumbrance,
        };
        assert!(!excepted.confirms());
        assert_eq!(excepted.code(), "excepted");
        assert_eq!(
            ReconciliationException::UnsupportedRepoEncumbrance.code(),
            "unsupported_repo_encumbrance"
        );
    }
}
