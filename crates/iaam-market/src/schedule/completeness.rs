//! Структурные инварианты полноты графика (§2.10, §2.11).
//!
//! Источник не даёт ни курсора, ни счётчика записей, поэтому сверить
//! количество не с чем. Полнота доказывается структурно, и все три
//! инварианта проверены живой выборкой из 50 бумаг TQOB и TQCB — 50/50
//! по каждому.
//!
//! Инварианты принадлежат **профилю источника**, а не домену, и имеют
//! явную область применимости: бескупонные, бессрочные и юридически
//! нестандартные выпуски в выборку не попали.

use rust_decimal::Decimal;
use time::Date;

use iaam_core::numeric::decimal::Dec;

// `CouponAmount` и `Knowledge` здесь не нужны: инварианты смотрят на
// даты и доли, а не на суммы. Тестам они нужны — и импортируются в блоке
// тестов, а не тут.
use crate::schedule::{CouponPeriod, PrincipalRepayment};

/// Итог структурной проверки.
///
/// `Incomplete` вместо `complete_prefix` намеренно: успешно скачанный,
/// но усечённый график выглядит замкнутым и правдоподобным, и «полный
/// префикс» звучит как «почти всё в порядке».
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Validated,
    Incomplete {
        reason: String,
    },
    /// Выпуск вне области применимости профиля.
    Unknown,
}

/// Проверить три инварианта профиля MOEX.
#[must_use]
pub fn validate_moex_profile(
    coupons: &[CouponPeriod],
    repayments: &[PrincipalRepayment],
) -> Completeness {
    if coupons.is_empty() || repayments.is_empty() {
        // Ни бескупонного, ни бессрочного выпуска в выборке не было.
        // Отвергнуть их корректный график — такая же ошибка, как принять
        // усечённый, поэтому здесь незнание, а не отказ.
        return Completeness::Unknown;
    }

    // Инвариант 1: цепь купонных периодов замкнута.
    for pair in coupons.windows(2) {
        if pair[0].accrual_end != pair[1].period_start {
            return Completeness::Incomplete {
                reason: format!(
                    "разрыв цепи периодов: период кончается {}, следующий начинается {}",
                    pair[0].accrual_end, pair[1].period_start
                ),
            };
        }
    }

    // Инвариант 2: хвост совпадает с последним возвратом номинала.
    // Ловит именно усечённую страницу: обрыв после целого периода
    // оставляет цепь замкнутой, и больше его ничто не замечает.
    let last_accrual = coupons
        .iter()
        .map(|period| period.accrual_end)
        .max()
        .unwrap_or(Date::MIN);
    let last_return = repayments
        .iter()
        .map(|repayment| repayment.repayment_date)
        .max()
        .unwrap_or(Date::MIN);
    if last_accrual != last_return {
        return Completeness::Incomplete {
            reason: format!(
                "хвост графика {last_accrual} не сходится с последним возвратом {last_return}"
            ),
        };
    }

    // Инвариант 3: доли возвратов суммируются ровно в 100 %.
    // Сложение через Dec::sum, а не через сырой Decimal: переполнение
    // и потеря точности здесь — отказ, а не тихо неверная сумма.
    let shares = repayments
        .iter()
        .map(|repayment| repayment.share_percent)
        .collect::<Vec<_>>();
    let total = match Dec::sum(&shares) {
        Ok(total) => total,
        Err(error) => {
            return Completeness::Incomplete {
                reason: format!("доли возвратов номинала не суммируются: {error}"),
            };
        }
    };
    if total != Dec::new(Decimal::from(100)) {
        return Completeness::Incomplete {
            reason: format!("доли возвратов номинала дают {}, а не 100", total.inner()),
        };
    }

    Completeness::Validated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{CouponAmount, Knowledge};
    use rust_decimal::Decimal;
    use time::macros::date;

    fn coupon(start: Date, end: Date) -> CouponPeriod {
        CouponPeriod {
            period_start: start,
            accrual_end: end,
            payment_date: end,
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        }
    }

    fn repayment(date: Date, share: i64) -> PrincipalRepayment {
        PrincipalRepayment {
            repayment_date: date,
            share_percent: Dec::new(Decimal::from(share)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        }
    }

    #[test]
    fn a_whole_schedule_validates() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 08 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        assert_eq!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Validated
        );
    }

    #[test]
    fn a_truncated_page_is_caught_by_the_tail_not_by_the_chain() {
        // Это главная ловушка: усечённая страница обрывается после целого
        // периода, цепь остаётся замкнутой, и график выглядит полным.
        // Ловит его только совпадение хвоста с последним возвратом.
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2036 - 02 - 06), 100)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn a_broken_chain_is_named_as_such() {
        let coupons = vec![
            coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15)),
            coupon(date!(2026 - 09 - 15), date!(2027 - 02 - 15)),
        ];
        let repayments = vec![repayment(date!(2027 - 02 - 15), 100)];
        let outcome = validate_moex_profile(&coupons, &repayments);
        let Completeness::Incomplete { reason } = outcome else {
            panic!("разрыв цепи обязан быть замечен: {outcome:?}");
        };
        assert!(
            reason.contains("2026-09-15"),
            "причина обязана назвать место: {reason}"
        );
    }

    #[test]
    fn shares_that_do_not_sum_to_a_hundred_are_incomplete() {
        let coupons = vec![coupon(date!(2026 - 02 - 15), date!(2026 - 08 - 15))];
        let repayments = vec![repayment(date!(2026 - 08 - 15), 75)];
        assert!(matches!(
            validate_moex_profile(&coupons, &repayments),
            Completeness::Incomplete { .. }
        ));
    }

    #[test]
    fn an_issue_outside_the_profile_is_unknown_not_rejected() {
        // Инварианты проверены на купонных выпусках с погашением.
        // Бескупонные и бессрочные в выборку не попали, и отвергнуть их
        // корректный график — такая же ошибка, как принять усечённый.
        assert_eq!(validate_moex_profile(&[], &[]), Completeness::Unknown);
    }
}
