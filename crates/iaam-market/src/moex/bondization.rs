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

    // Эталоны сняты живыми вызовами ISS 2026-08-27 и заморожены
    // (`tests/fixtures/MANIFEST.sha256`). Литерал, сконструированный
    // по памяти, проверяет наше представление об источнике, а не сам
    // источник, — и расходится с ним молча.
    const FLOATER: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-floater.json");
    const AMORTISED: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-amortised.json");
    const OFFERS: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-offers.json");
    const FIXED_COUPON: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-fixed-coupon.json");
    const FOREIGN_FACE: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-foreign-face.json");
    const PAGE_ONE: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-page-1.json");
    const PAGE_TWO: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/moex-iss-bondization-page-2.json");

    fn parsed(body: &[u8]) -> BondizationPage {
        parse_bondization_page(body, ObservedAt(datetime!(2026-08-27 12:00:00 UTC)))
            .expect("страница разобрана")
    }

    #[test]
    fn a_missing_amount_stays_unknown_and_does_not_become_zero() {
        // У проверенного флоатера прошедший купон приходит без суммы и без
        // ставки. Ноль здесь занизил бы и поток, и YTM, и сделал бы это
        // правдоподобно.
        let page = parsed(FLOATER);
        assert_eq!(page.coupon_periods[0].amount, CouponAmount::Undetermined);
    }

    #[test]
    fn a_known_amount_carries_its_currency() {
        let page = parsed(FLOATER);
        assert!(matches!(
            page.coupon_periods[1].amount,
            CouponAmount::AmountFixed { .. }
        ));
    }

    #[test]
    fn a_foreign_face_value_keeps_its_own_currency() {
        // Валюта номинала — не рубль по умолчанию. Подставить рубль значит
        // сложить доллары с рублями и получить правдоподобное число.
        let page = parsed(FOREIGN_FACE);
        let CouponAmount::AmountFixed { currency, .. } = page.coupon_periods[0].amount else {
            panic!("сумма купона известна: {:?}", page.coupon_periods[0].amount);
        };
        assert_eq!(currency.code(), "USD");
    }

    #[test]
    fn the_source_kind_arrives_uninterpreted() {
        // Разборщик кодов не толкует: вид права по оферте у MOEX это
        // свободный русский текст, и match по нему сломается от правки
        // формулировки на стороне биржи.
        assert_eq!(
            parsed(AMORTISED).principal_repayments[0].source_kind,
            "amortization"
        );
        assert_eq!(parsed(OFFERS).offer_windows[0].source_kind, "Оферта");
        assert_eq!(
            parsed(FIXED_COUPON).principal_repayments[0].source_kind,
            "maturity"
        );
    }

    #[test]
    fn the_row_face_value_is_ignored_entirely() {
        // Поле номинала в строке — номинал бумаги НА МОМЕНТ ЗАПРОСА:
        // у бумаги, прошедшей часть амортизаций, все строки за все годы
        // показывают текущий остаток. Принять его за номинал периода
        // значит задним числом пересчитать всю историю.
        let page = parsed(AMORTISED);
        // Возврат несёт долю первоначального номинала, а не сумму,
        // выведенную из показанного номинала строки.
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
        // У одного и того же выпуска окна приходят и с ценой, и без неё.
        // Пустая цена — незнание условий, а не выкуп даром.
        let page = parsed(OFFERS);
        assert!(matches!(
            page.offer_windows[0].price_percent,
            Knowledge::Known(_)
        ));
        assert!(matches!(
            page.offer_windows[1].price_percent,
            Knowledge::Unknown
        ));
    }

    #[test]
    fn the_first_page_of_a_long_issue_looks_whole_and_is_not() {
        // Эталон конкретной ловушки: первая страница замкнута по цепи и
        // короче настоящего графика на десять лет. Ловит её только
        // несовпадение хвоста с последним возвратом номинала.
        let page = parsed(PAGE_ONE);
        assert_eq!(page.coupon_periods.len(), 100);
        let outcome = crate::schedule::completeness::validate_moex_profile(
            &page.coupon_periods,
            &page.principal_repayments,
        );
        assert!(
            matches!(
                outcome,
                crate::schedule::completeness::Completeness::Incomplete { .. }
            ),
            "усечённая страница обязана давать Incomplete: {outcome:?}"
        );
    }

    #[test]
    fn the_second_page_closes_the_chain_the_first_left_open() {
        // Вторая страница продолжает тот же ряд: её первый период
        // начинается там, где кончился последний период первой, и вместе
        // они дают полный график. Это и есть доказательство, что
        // остановка на первой странице была усечением.
        let first = parsed(PAGE_ONE);
        let second = parsed(PAGE_TWO);
        assert_eq!(
            first
                .coupon_periods
                .last()
                .expect("хвост первой")
                .accrual_end,
            second.coupon_periods[0].period_start
        );
        let mut coupons = first.coupon_periods;
        coupons.extend(second.coupon_periods);
        let mut repayments = first.principal_repayments;
        repayments.extend(second.principal_repayments);
        assert_eq!(
            crate::schedule::completeness::validate_moex_profile(&coupons, &repayments),
            crate::schedule::completeness::Completeness::Validated
        );
    }

    #[test]
    fn a_rate_without_an_amount_is_not_a_zero_amount() {
        // Литерал, а не эталон, намеренно: MOEX такую строку не отдаёт
        // ни в одном из проверенных выпусков — он заполняет `value`
        // и `valueprc` вместе либо не заполняет ни одного. Состояние
        // «ставка известна, сумма ещё нет» объявлено спекой (§2.3) и
        // хранится схемой, поэтому разбор обязан его строить, а не
        // схлопывать в `Undetermined`: схлопывание потеряло бы
        // известную ставку флоатера.
        const RATE_ONLY: &str = r#"{
          "amortizations": {"columns": ["amortdate", "valueprc", "data_source"], "data": []},
          "coupons": {
            "columns": ["coupondate", "startdate", "value", "valueprc", "faceunit"],
            "data": [["2027-02-15", "2026-08-15", null, 6.9, "RUB"]]
          },
          "offers": {"columns": ["offerdate", "offertype"], "data": []}
        }"#;
        let page = parsed(RATE_ONLY.as_bytes());
        let CouponAmount::RateFixedAmountUndetermined { rate_percent } =
            &page.coupon_periods[0].amount
        else {
            panic!(
                "ставка известна, сумма нет: {:?}",
                page.coupon_periods[0].amount
            );
        };
        assert_eq!(rate_percent.inner().to_string(), "6.9");
    }

    #[test]
    fn the_row_count_covers_all_three_blocks_together() {
        // Счётчик строк — единственное, по чему вызывающий отличает
        // «страница пуста» от «пуст один блок». Счёт по одному блоку
        // остановил бы пагинацию там, где кончились амортизации,
        // и обрезал бы купоны.
        let page = parsed(OFFERS);
        assert_eq!(page.coupon_periods.len(), 40);
        assert_eq!(page.principal_repayments.len(), 1);
        assert_eq!(page.offer_windows.len(), 8);
        assert_eq!(page.total_rows, 49);
    }
}
