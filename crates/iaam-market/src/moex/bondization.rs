//! Разбор графика выплат MOEX ISS.
//!
//! Ответ табличный: `columns` с именами и `data` со строками. Индексы
//! берутся из `columns` по имени, а не зашиваются числами: ISS добавляет
//! колонки, и позиционный разбор однажды прочитает долю как дату.
//!
//! Разборщик **не толкует коды**. Вид возврата номинала и вид права по
//! оферте доходят до домена как есть; перевод — словарём (§2.5). У MOEX
//! вид права это свободный русский текст, и `match` по нему сломался бы
//! от правки формулировки на стороне биржи.
//!
//! Поле номинала в строке игнорируется целиком: это номинал бумаги на
//! момент запроса, а не номинал периода (§2.11).

use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use iaam_core::numeric::decimal::Dec;

use crate::error::MarketError;
use crate::observation::ObservedAt;
use crate::schedule::{CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment};

/// Одна страница ответа. Пагинация — забота вызывающего (§2.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondizationPage {
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
    /// Сколько строк пришло во всех блоках вместе.
    ///
    /// Нужно вызывающему, чтобы отличить «страница пуста» от «блок пуст»:
    /// смещение у блоков общее, и амортизации кончаются раньше купонов.
    pub total_rows: usize,
}

fn block<'a>(root: &'a Value, name: &str) -> Result<(&'a Vec<Value>, Vec<String>), MarketError> {
    let node = root
        .get(name)
        .ok_or_else(|| MarketError::Malformed(format!("нет блока {name}")))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("нет columns у {name}")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| MarketError::Malformed(format!("имя колонки {name} не строка")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("нет data у {name}")))?;
    Ok((data, columns))
}

fn cell<'a>(columns: &[String], row: &'a Value, name: &str) -> Option<&'a Value> {
    let index = columns.iter().position(|column| column == name)?;
    let value = row.get(index)?;
    if value.is_null() { None } else { Some(value) }
}

fn date_of(columns: &[String], row: &Value, name: &str) -> Result<Option<Date>, MarketError> {
    let Some(value) = cell(columns, row, name) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| MarketError::Malformed(format!("{name} не строка")))?;
    Date::parse(text, &Iso8601::DATE)
        .map(Some)
        .map_err(|_| MarketError::Malformed(format!("{name} не дата: {text}")))
}

fn required_date(columns: &[String], row: &Value, name: &str) -> Result<Date, MarketError> {
    date_of(columns, row, name)?
        .ok_or_else(|| MarketError::Malformed(format!("{name} обязателен и пуст")))
}

fn decimal_of(columns: &[String], row: &Value, name: &str) -> Result<Option<Dec>, MarketError> {
    let Some(value) = cell(columns, row, name) else {
        return Ok(None);
    };
    let text = if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        value.to_string()
    };
    text.parse::<Decimal>()
        .map(|decimal| Some(Dec::new(decimal)))
        .map_err(|_| MarketError::Malformed(format!("{name} не число: {text}")))
}

fn text_of(columns: &[String], row: &Value, name: &str) -> Option<String> {
    cell(columns, row, name).and_then(|value| value.as_str().map(str::to_owned))
}

fn knowledge<T>(value: Option<T>) -> Knowledge<T> {
    value.map_or(Knowledge::Unknown, Knowledge::Known)
}

/// Разобрать одну страницу ответа `/bondization`.
///
/// `observed_at` назначает вызывающий: доверить ось знания часам источника
/// значит сделать её подделываемой ответом.
pub fn parse_bondization_page(
    body: &[u8],
    _observed_at: ObservedAt,
) -> Result<BondizationPage, MarketError> {
    let root: Value =
        serde_json::from_slice(body).map_err(|error| MarketError::Malformed(error.to_string()))?;

    let (coupon_rows, coupon_columns) = block(&root, "coupons")?;
    let mut coupon_periods = Vec::with_capacity(coupon_rows.len());
    for row in coupon_rows {
        let period_start = required_date(&coupon_columns, row, "startdate")?;
        // Конец начисления и дата платежа — разные смыслы. Источник даёт
        // одно значение на оба; различие сохраняется, потому что перенос
        // с выходного двигает вторую, но не первый.
        let coupon_date = required_date(&coupon_columns, row, "coupondate")?;
        let per_unit = decimal_of(&coupon_columns, row, "value")?;
        let rate_percent = decimal_of(&coupon_columns, row, "valueprc")?;
        let currency = text_of(&coupon_columns, row, "faceunit");
        // Поля толкуются независимо: null у суммы не делает нулём ставку.
        let amount = match (per_unit, rate_percent, currency) {
            (Some(per_unit), _, Some(code)) => CouponAmount::AmountFixed {
                per_unit,
                currency: crate::moex::parse::currency_of(&code)?,
            },
            (None, Some(rate_percent), _) => {
                CouponAmount::RateFixedAmountUndetermined { rate_percent }
            }
            _ => CouponAmount::Undetermined,
        };
        coupon_periods.push(CouponPeriod {
            period_start,
            accrual_end: coupon_date,
            payment_date: coupon_date,
            record_date: knowledge(date_of(&coupon_columns, row, "recorddate")?),
            amount,
            source_entry_id: None,
        });
    }

    let (amort_rows, amort_columns) = block(&root, "amortizations")?;
    let mut principal_repayments = Vec::with_capacity(amort_rows.len());
    for row in amort_rows {
        principal_repayments.push(PrincipalRepayment {
            repayment_date: required_date(&amort_columns, row, "amortdate")?,
            share_percent: decimal_of(&amort_columns, row, "valueprc")?
                .ok_or_else(|| MarketError::Malformed("возврат номинала без доли".to_owned()))?,
            source_kind: text_of(&amort_columns, row, "data_source")
                .ok_or_else(|| MarketError::Malformed("возврат номинала без вида".to_owned()))?,
            source_entry_id: None,
        });
    }

    let (offer_rows, offer_columns) = block(&root, "offers")?;
    let mut offer_windows = Vec::with_capacity(offer_rows.len());
    for row in offer_rows {
        offer_windows.push(OfferWindow {
            execution_date: required_date(&offer_columns, row, "offerdate")?,
            submission_start: knowledge(date_of(&offer_columns, row, "offerdatestart")?),
            submission_end: knowledge(date_of(&offer_columns, row, "offerdateend")?),
            price_percent: knowledge(decimal_of(&offer_columns, row, "price")?),
            agent: knowledge(text_of(&offer_columns, row, "agent")),
            source_kind: text_of(&offer_columns, row, "offertype")
                .ok_or_else(|| MarketError::Malformed("окно оферты без вида".to_owned()))?,
            source_entry_id: None,
        });
    }

    let total_rows = coupon_periods.len() + principal_repayments.len() + offer_windows.len();
    Ok(BondizationPage {
        coupon_periods,
        principal_repayments,
        offer_windows,
        total_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const PAGE: &str = r#"{
      "amortizations": {
        "columns": ["amortdate", "facevalue", "initialfacevalue", "valueprc",
                    "value", "value_rub", "data_source"],
        "data": [["2034-08-09", 375, 1000, 25, 250, 250, "amortization"]]
      },
      "coupons": {
        "columns": ["coupondate", "recorddate", "startdate", "initialfacevalue",
                    "facevalue", "faceunit", "value", "valueprc", "value_rub"],
        "data": [
          ["2026-08-15", null, "2026-02-15", 1000, 375, "RUB", null, null, null],
          ["2027-02-15", "2027-02-14", "2026-08-15", 1000, 375, "RUB", 34.41, 6.9, 34.41]
        ]
      },
      "offers": {
        "columns": ["offerdate", "offerdatestart", "offerdateend", "price",
                    "value", "agent", "offertype"],
        "data": [["2027-08-26", null, null, null, null, null, "Оферта"]]
      }
    }"#;

    fn parsed() -> BondizationPage {
        parse_bondization_page(
            PAGE.as_bytes(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .expect("страница разобрана")
    }

    #[test]
    fn a_missing_amount_stays_unknown_and_does_not_become_zero() {
        // У проверенного флоатера прошедший купон приходит без суммы и без
        // ставки. Ноль здесь занизил бы и поток, и YTM, и сделал бы это
        // правдоподобно.
        let page = parsed();
        assert_eq!(page.coupon_periods[0].amount, CouponAmount::Undetermined);
    }

    #[test]
    fn a_known_amount_carries_its_currency() {
        let page = parsed();
        assert!(matches!(
            page.coupon_periods[1].amount,
            CouponAmount::AmountFixed { .. }
        ));
    }

    #[test]
    fn the_source_kind_arrives_uninterpreted() {
        // Разборщик кодов не толкует: вид права по оферте у MOEX это
        // свободный русский текст, и match по нему сломается от правки
        // формулировки на стороне биржи.
        let page = parsed();
        assert_eq!(page.principal_repayments[0].source_kind, "amortization");
        assert_eq!(page.offer_windows[0].source_kind, "Оферта");
    }

    #[test]
    fn the_row_face_value_is_ignored_entirely() {
        // Поле номинала в строке — номинал бумаги НА МОМЕНТ ЗАПРОСА:
        // у бумаги, прошедшей часть амортизаций, все строки за все годы
        // показывают текущий остаток. Принять его за номинал периода
        // значит задним числом пересчитать всю историю.
        let page = parsed();
        // Возврат несёт долю первоначального номинала, а не сумму,
        // выведенную из показанных 375.
        assert_eq!(
            page.principal_repayments[0]
                .share_percent
                .inner()
                .to_string(),
            "25"
        );
    }

    #[test]
    fn an_offer_without_conditions_is_unknown() {
        let page = parsed();
        assert!(matches!(
            page.offer_windows[0].price_percent,
            Knowledge::Unknown
        ));
    }
}
