//! Окончательность возврата номинала (§6 спеки E3.4.4, бид iaam-d8b.4.3).
//!
//! Правило одно: возврат окончателен, когда накопленная сумма долей
//! достигает 100 %. Код источника не читается — у шести бумаг из
//! пятидесяти проверенных строки погашения нет вовсе.
//!
//! Признак наблюдением не записывается: он свойство проекции (ADR-0002).
//! Инвариант полноты в `iaam_market::schedule::completeness` считает ту
//! же сумму, но принадлежит ПРОФИЛЮ ИСТОЧНИКА и отвечает на другой
//! вопрос — цела ли выгрузка.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::bond::PrincipalReturn;
use crate::numeric::NumericError;
use crate::numeric::decimal::Dec;

/// Окончателен ли возврат номинала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalReturnFinality {
    /// Накопленная доля достигла 100 %: номинал возвращён целиком.
    Final,
    /// Часть номинала, после которой останется непогашенный остаток.
    Partial,
    /// Доли не дают 100 %: сказать нечего ни про одну строку.
    Unknown,
}

/// Разметить ряд возвратов признаком окончательности.
pub fn finality_of(
    returns: &[PrincipalReturn],
) -> Result<Vec<(PrincipalReturn, PrincipalReturnFinality)>, NumericError> {
    let shares = returns.iter().map(|r| r.share_percent).collect::<Vec<_>>();
    let total = Dec::sum(&shares)?;
    let hundred = Dec::new(Decimal::ONE_HUNDRED);
    if total != hundred {
        return Ok(returns
            .iter()
            .map(|r| (*r, PrincipalReturnFinality::Unknown))
            .collect());
    }

    // Порядок источника не гарантирован, а накопление зависит от него
    // целиком: без сортировки окончательной окажется случайная строка.
    let mut ordered = returns.to_vec();
    ordered.sort_by_key(|r| r.repayment_date);

    let mut accumulated = Dec::zero();
    let mut marked = Vec::with_capacity(ordered.len());
    for item in ordered {
        accumulated = accumulated.checked_add(item.share_percent)?;
        let finality = if accumulated == hundred {
            PrincipalReturnFinality::Final
        } else {
            PrincipalReturnFinality::Partial
        };
        marked.push((item, finality));
    }
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bond::PrincipalReturn;
    use rust_decimal::Decimal;
    use time::{Date, macros::date};

    fn ret(day: Date, share: &str) -> PrincipalReturn {
        PrincipalReturn {
            repayment_date: day,
            share_percent: Dec::new(Decimal::from_str_exact(share).unwrap()),
        }
    }

    #[test]
    fn six_amortisations_without_a_maturity_code_still_end_finally() {
        // У шести бумаг из пятидесяти проверенных последний возврат
        // приходит обычной строкой амортизации, без кода погашения.
        // Читать код источника значит потерять окончательность у них.
        let returns = vec![
            ret(date!(2027 - 01 - 15), "10"),
            ret(date!(2028 - 01 - 15), "10"),
            ret(date!(2029 - 01 - 15), "10"),
            ret(date!(2030 - 01 - 15), "20"),
            ret(date!(2031 - 01 - 15), "20"),
            ret(date!(2032 - 01 - 15), "30"),
        ];
        let marked = finality_of(&returns).unwrap();
        assert_eq!(marked[5].1, PrincipalReturnFinality::Final);
        assert_eq!(marked[4].1, PrincipalReturnFinality::Partial);
    }

    #[test]
    fn shares_short_of_a_hundred_make_nobody_final() {
        // Усечённая страница даёт правдоподобный, но неполный ряд.
        // Объявить последнюю строку окончательной значит закрыть
        // бумагу на десять лет раньше срока.
        let returns = vec![
            ret(date!(2027 - 01 - 15), "40"),
            ret(date!(2028 - 01 - 15), "35"),
        ];
        let marked = finality_of(&returns).unwrap();
        assert!(
            marked
                .iter()
                .all(|(_, finality)| *finality == PrincipalReturnFinality::Unknown)
        );
    }

    #[test]
    fn returns_are_walked_in_date_order_not_in_source_order() {
        // Источник порядок строк не гарантирует, а накопление доли
        // от порядка зависит целиком.
        let returns = vec![
            ret(date!(2028 - 01 - 15), "60"),
            ret(date!(2027 - 01 - 15), "40"),
        ];
        let marked = finality_of(&returns).unwrap();
        let final_one = marked
            .iter()
            .find(|(_, finality)| *finality == PrincipalReturnFinality::Final)
            .unwrap();
        assert_eq!(final_one.0.repayment_date, date!(2028 - 01 - 15));
    }
}
