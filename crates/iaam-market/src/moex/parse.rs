//! Разбор ответа ISS.
//!
//! Ответ приходит табличным: массив `columns` с именами и массив `data`
//! со строками. Индексы колонок берутся из `columns` по имени, а не
//! зашиваются числами: ISS добавляет колонки, и позиционный разбор
//! однажды прочитает объём как цену.

use iaam_core::ids::InstrumentId;
use iaam_core::money::{CurrencyCode, PerUnitAmount};
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::QuotationBasis;
use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use crate::error::MarketError;
use crate::observation::{
    AccruedInterestObservation, Executability, ObservedAt, PriceKind, PriceObservation, TradeDate,
    Venue,
};

/// Ценовые колонки ISS и их смысл.
///
/// Все шесть равноправны: выбор между ними — политика оценки (E3.3).
const PRICE_COLUMNS: [(&str, PriceKind); 6] = [
    ("CLOSE", PriceKind::Close),
    ("LEGALCLOSEPRICE", PriceKind::LegalClose),
    ("WAPRICE", PriceKind::WeightedAverage),
    ("MARKETPRICE2", PriceKind::MarketPrice2),
    ("MARKETPRICE3", PriceKind::MarketPrice3),
    ("ADMITTEDQUOTE", PriceKind::AdmittedQuote),
];

/// Код валюты источника в доменный код.
///
/// `SUR` — код советского рубля из старого стандарта, который биржа
/// не меняла. Без этого отображения разбор либо падает на каждой
/// рублёвой бумаге, либо заводит вторую валюту рядом с рублём,
/// и позиции разъезжаются по двум валютам с одним смыслом.
pub(crate) fn currency_of(code: &str) -> Result<CurrencyCode, MarketError> {
    match code {
        "SUR" | "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        other => Err(MarketError::UnknownCurrency(other.to_owned())),
    }
}

/// Сегмент ISS, из которого взята строка котировки.
///
/// Это те же `engine` и `market`, из которых собран путь запроса
/// (`super::history_request`), поэтому основание известно адаптеру заранее.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketSegment<'a> {
    pub engine: &'a str,
    pub market: &'a str,
}

impl MarketSegment<'_> {
    /// Основание котировки и признак, по которому оно выведено.
    ///
    /// Таблица описывает пары пути запроса, а не род инструмента.
    /// Незнакомая пара остаётся неизвестной, чтобы не выдать догадку
    /// за доказанное денежное значение.
    #[must_use]
    pub fn quotation_basis(self) -> (QuotationBasis, String) {
        let basis = match (self.engine, self.market) {
            ("stock", "bonds") => QuotationBasis::PercentOfRemainingFace,
            ("stock", "shares") => QuotationBasis::MoneyPerUnit,
            _ => QuotationBasis::Unknown,
        };
        (basis, self.evidence())
    }

    fn evidence(self) -> String {
        format!("iss:engines/{}/markets/{}", self.engine, self.market)
    }
}

/// Разбор страницы истории в наблюдения.
///
/// `observed_at` приходит **снаружи**: в ответе ISS момента наблюдения
/// нет вовсе, и назначать его обязана система.
pub fn parse_history(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
    segment: MarketSegment<'_>,
) -> Result<Vec<PriceObservation>, MarketError> {
    let (basis, basis_evidence) = segment.quotation_basis();
    let root: Value =
        serde_json::from_str(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let block = root
        .get("history")
        .ok_or_else(|| MarketError::Malformed("нет блока history".to_owned()))?;
    let names = column_names(block)?;
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет history.data".to_owned()))?;

    ensure_page_is_whole(&root, rows.len())?;

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("строка history.data не массив".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без TRADEDATE".to_owned()))?,
        )?);
        let currency = currency_of(
            get("CURRENCYID")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без CURRENCYID".to_owned()))?,
        )?;
        let venue = Venue {
            board: get("BOARDID")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без BOARDID".to_owned()))?
                .to_owned(),
            session: get("TRADINGSESSION").and_then(Value::as_i64).unwrap_or(0),
        };
        for (column, kind) in PRICE_COLUMNS {
            // Пустая колонка наблюдения не порождает: отсутствующее
            // значение это Option, а не ноль (§4.9). Ноль в цене
            // означал бы «бумага ничего не стоит».
            let Some(value) = get(column) else {
                continue;
            };
            let Some(number) = value.as_number() else {
                if value.is_null() {
                    continue;
                }
                return Err(MarketError::Malformed(format!("колонка {column} не число")));
            };
            let price = number
                .to_string()
                .parse::<Decimal>()
                .map_err(|error| MarketError::Malformed(error.to_string()))?;
            observations.push(PriceObservation {
                instrument,
                venue: venue.clone(),
                trade_date,
                observed_at,
                kind,
                price: Dec::new(price),
                currency,
                basis,
                basis_evidence: basis_evidence.clone(),
                // Дневная история даёт цену закрытия, а не исполнимый bid.
                // Помечать её исполнимой значило бы выдать ориентир
                // за цену выхода (§5.1, §5.3).
                executability: Executability::IndicativePreviousClose,
            });
        }
    }
    Ok(observations)
}

/// Разбор наблюдений НКД из той же страницы истории.
///
/// Отдельная функция, а не ветка внутри `parse_history`: величины разной
/// размерности (процент номинала против денег) и разной судьбы —
/// смешивать их в одном цикле значит однажды записать одну вместо другой.
pub fn parse_accrued_interest(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<Vec<AccruedInterestObservation>, MarketError> {
    let root: Value =
        serde_json::from_str(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let block = root
        .get("history")
        .ok_or_else(|| MarketError::Malformed("нет блока history".to_owned()))?;
    let names = column_names(block)?;
    // Колонки нет вовсе — это не облигационный сегмент, а не поломка.
    if index_of(&names, "ACCINT").is_none() {
        return Ok(Vec::new());
    }
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет history.data".to_owned()))?;

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("строка history.data не массив".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        // Пустое значение наблюдения не порождает: ноль НКД означал бы
        // начало купонного периода, а не отсутствие торгов.
        let Some(value) = get("ACCINT").and_then(Value::as_number) else {
            continue;
        };
        let amount = value
            .to_string()
            .parse::<Decimal>()
            .map_err(|error| MarketError::Malformed(error.to_string()))?;
        // Валюта НКД — валюта номинала (FACEUNIT), а не валюта расчётов
        // площадки (CURRENCYID). В одной строке они различаются.
        let currency =
            currency_of(get("FACEUNIT").and_then(Value::as_str).ok_or_else(|| {
                MarketError::Malformed("строка с ACCINT без FACEUNIT".to_owned())
            })?)?;
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("строка без TRADEDATE".to_owned()))?,
        )?);
        observations.push(AccruedInterestObservation {
            instrument,
            venue: Venue {
                board: get("BOARDID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| MarketError::Malformed("строка без BOARDID".to_owned()))?
                    .to_owned(),
                session: get("TRADINGSESSION").and_then(Value::as_i64).unwrap_or(0),
            },
            trade_date,
            observed_at,
            per_unit: PerUnitAmount::new(Dec::new(amount), currency),
        });
    }
    Ok(observations)
}

/// Страница пришла целиком.
///
/// Курсор ISS даёт `INDEX`, `TOTAL` и `PAGESIZE`. Неполная страница,
/// принятая за полную, даёт пробел в ряду, который потом невозможно
/// отличить от нерабочего дня — то есть тихую порчу истории.
fn ensure_page_is_whole(root: &Value, got: usize) -> Result<(), MarketError> {
    let Some(cursor) = root.get("history.cursor") else {
        return Ok(());
    };
    let names = column_names(cursor)?;
    let Some(row) = cursor
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let value = |name: &str| index_of(&names, name).and_then(|i| row.get(i)?.as_u64());
    let (Some(index), Some(total), Some(page)) =
        (value("INDEX"), value("TOTAL"), value("PAGESIZE"))
    else {
        return Ok(());
    };
    let expected = usize::try_from(total.saturating_sub(index))
        .unwrap_or(usize::MAX)
        .min(usize::try_from(page).unwrap_or(usize::MAX));
    if got < expected {
        return Err(MarketError::Truncated {
            got,
            total: usize::try_from(total).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

fn column_names(block: &Value) -> Result<Vec<String>, MarketError> {
    block
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("нет columns".to_owned()))?
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_owned)
                .ok_or_else(|| MarketError::Malformed("имя колонки не строка".to_owned()))
        })
        .collect()
}

fn index_of(names: &[String], name: &str) -> Option<usize> {
    names.iter().position(|candidate| candidate == name)
}

fn parse_date(value: &str) -> Result<Date, MarketError> {
    Date::parse(value, &Iso8601::DATE)
        .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
}
#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::money::CurrencyCode;
    use iaam_core::valuation::QuotationBasis;

    const BONDS: MarketSegment<'static> = MarketSegment {
        engine: "stock",
        market: "bonds",
    };
    const SHARES: MarketSegment<'static> = MarketSegment {
        engine: "stock",
        market: "shares",
    };
    use time::macros::{date, datetime};

    const FIXTURE: &str =
        include_str!("../../../../tests/fixtures/market/moex-iss-history-sber.json");

    const BOND_HISTORY: &str = r#"{"history":{
        "columns":["BOARDID","TRADEDATE","SECID","CLOSE","ACCINT","CURRENCYID","FACEUNIT","TRADINGSESSION"],
        "data":[
            ["TQOB","2026-08-20","SU26238RMFS4",53.198,15.17,"SUR","RUB",3],
            ["TQOB","2026-08-21","SU26238RMFS4",53.355,null,"SUR","RUB",3]
        ]}}"#;

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    fn instrument() -> InstrumentId {
        InstrumentId::new_random()
    }

    #[test]
    fn moex_reports_the_rouble_as_sur_and_it_resolves_to_rub() {
        // SUR — код советского рубля из старого стандарта, который биржа
        // не меняла. Разбор, не знающий этого, либо падает, либо заводит
        // вторую валюту рядом с рублём.
        assert_eq!(currency_of("SUR").expect("рубль"), CurrencyCode::Rub);
    }

    #[test]
    fn an_unknown_currency_is_named_rather_than_swallowed() {
        assert!(matches!(
            currency_of("ZZZ"),
            Err(MarketError::UnknownCurrency(code)) if code == "ZZZ"
        ));
    }

    #[test]
    fn one_row_yields_one_observation_per_non_empty_price_column() {
        let observations =
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("разбор фикстуры");
        let first_day: Vec<_> = observations
            .iter()
            .filter(|o| o.trade_date == TradeDate(date!(2026 - 08 - 03)))
            .collect();
        // В фикстуре у первой строки ADMITTEDQUOTE пуст, остальные пять
        // колонок заполнены.
        assert_eq!(
            first_day.len(),
            5,
            "ожидалось пять наблюдений на день, получено {}",
            first_day.len()
        );
        assert!(
            !first_day.iter().any(|o| o.kind == PriceKind::AdmittedQuote),
            "пустая колонка не должна порождать наблюдение"
        );
    }

    #[test]
    fn the_venue_and_session_travel_with_the_observation() {
        let observations =
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("разбор фикстуры");
        let first = observations.first().expect("хотя бы одно наблюдение");
        assert_eq!(first.venue.board, "TQBR");
        assert_eq!(first.venue.session, 3);
        assert_eq!(first.currency, CurrencyCode::Rub);
    }

    #[test]
    fn the_knowledge_axis_comes_from_the_caller_not_the_response() {
        // В ответе ISS нет момента наблюдения вовсе. Он назначается
        // системой: доверить его источнику значит сделать ось знания
        // подделываемой ответом.
        let observations =
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("разбор фикстуры");
        assert!(observations.iter().all(|o| o.observed_at == observed()));
    }

    #[test]
    fn a_short_page_is_a_refusal_not_a_shorter_series() {
        let truncated = FIXTURE.replace("[0, 15, 100]", "[0, 40, 100]");
        assert!(matches!(
            parse_history(&truncated, instrument(), observed(), SHARES),
            Err(MarketError::Truncated { got: 15, total: 40 })
        ));
    }
    #[test]
    fn the_bond_market_quotes_in_percent_of_remaining_face() {
        let (basis, evidence) = BONDS.quotation_basis();
        assert_eq!(basis, QuotationBasis::PercentOfRemainingFace);
        assert_eq!(evidence, "iss:engines/stock/markets/bonds");
    }

    #[test]
    fn the_share_market_quotes_in_money_per_unit() {
        assert_eq!(SHARES.quotation_basis().0, QuotationBasis::MoneyPerUnit);
    }

    #[test]
    fn an_unfamiliar_market_does_not_default_to_money_per_unit() {
        // Неизвестный рынок котирует неизвестно как, а не по умолчанию деньгами.
        let segment = MarketSegment {
            engine: "currency",
            market: "selt",
        };
        assert_eq!(segment.quotation_basis().0, QuotationBasis::Unknown);
    }

    #[test]
    fn the_basis_comes_from_the_segment_not_from_the_response_body() {
        // Основание задаётся рынком, а не содержимым строки ответа.
        let instrument = InstrumentId::new_random();
        let observed_at = ObservedAt(datetime!(2026-08-21 19:00:00 UTC));
        let as_shares = parse_history(FIXTURE, instrument, observed_at, SHARES).unwrap();
        let as_bonds = parse_history(FIXTURE, instrument, observed_at, BONDS).unwrap();

        assert_eq!(as_shares[0].basis, QuotationBasis::MoneyPerUnit);
        assert_eq!(as_bonds[0].basis, QuotationBasis::PercentOfRemainingFace);
        assert_eq!(as_shares[0].price, as_bonds[0].price, "цена не меняется");
    }

    #[test]
    fn accrued_interest_takes_its_currency_from_face_unit_not_from_currency_id() {
        // В одной строке источник называет валюту дважды и по-разному:
        // CURRENCYID=SUR и FACEUNIT=RUB. НКД выражен в валюте номинала.
        let observations = parse_accrued_interest(
            BOND_HISTORY,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        assert_eq!(observations.len(), 1, "строка с null наблюдения не даёт");
        assert_eq!(observations[0].per_unit.currency(), CurrencyCode::Rub);
        assert_eq!(
            observations[0].per_unit.value(),
            Dec::new(Decimal::from_str_exact("15.17").unwrap())
        );
    }

    #[test]
    fn a_response_without_the_column_yields_nothing_rather_than_failing() {
        // Ответ по акции колонки ACCINT не содержит вовсе. Отказ здесь
        // сломал бы синхронизацию всех необлигаций.
        let body = r#"{"history":{"columns":["BOARDID","TRADEDATE","CLOSE","CURRENCYID","TRADINGSESSION"],
            "data":[["TQBR","2026-08-20",300.5,"SUR",3]]}}"#;
        let observations = parse_accrued_interest(
            body,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        assert!(observations.is_empty());
    }
}
