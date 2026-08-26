//! Разбор курсов ЦБ РФ.
//!
//! Две конвенции источника, которые легко пропустить и обе тихие:
//! ответ приходит в `windows-1251`, а десятичный разделитель — запятая.

use encoding_rs::WINDOWS_1251;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use rust_decimal::Decimal;
use time::{Date, Month, Weekday};

use crate::error::MarketError;
use crate::observation::{FxObservation, ObservedAt, TradeDate};

/// Одна сырая запись ЦБ до отображения в доменную валюту.
///
/// `char_code` остаётся строкой намеренно: справочник ЦБ шире
/// [`CurrencyCode`], и неизвестная системе валюта не должна ломать весь
/// ответ. Для динамического ответа в поле помещается идентификатор ЦБ,
/// потому что в элементах `Record` нет `CharCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CbrRate {
    pub char_code: String,
    pub nominal: u32,
    pub value: Decimal,
    pub unit_rate: Decimal,
    pub date: Date,
}

/// Байты ответа ЦБ в строку.
///
/// Отдельная функция, а не `String::from_utf8_lossy`: lossy подставил бы
/// вопросительные знаки вместо названий валют и сделал бы порчу
/// незаметной. `windows-1251` объявлена в прологе самого ответа.
#[must_use]
pub fn decode_cp1251(bytes: &[u8]) -> String {
    let (text, _, _) = WINDOWS_1251.decode(bytes);
    text.into_owned()
}

/// Число в конвенции ЦБ: десятичная запятая.
///
/// Точка отвергается намеренно. Принять оба разделителя значило бы
/// перестать замечать, что источник сменил конвенцию, — а смена
/// конвенции у источника истины обязана быть отказом, а не догадкой.
pub(crate) fn parse_cbr_decimal(value: &str) -> Result<Decimal, MarketError> {
    if !value.contains(',') && value.contains('.') {
        return Err(MarketError::Malformed(format!(
            "разделитель ЦБ — запятая, получено {value}"
        )));
    }
    value
        .replace(',', ".")
        .parse::<Decimal>()
        .map_err(|error| MarketError::Malformed(format!("число {value}: {error}")))
}

/// Дата в конвенции ЦБ: `DD.MM.YYYY`.
pub(crate) fn parse_cbr_date(value: &str) -> Result<Date, MarketError> {
    let parts: Vec<&str> = value.split('.').collect();
    let [day, month, year] = parts.as_slice() else {
        return Err(MarketError::Malformed(format!(
            "дата ЦБ ожидается как DD.MM.YYYY, получено {value}"
        )));
    };
    let parsed = |part: &str| {
        part.parse::<u16>()
            .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
    };
    let month = Month::try_from(u8::try_from(parsed(month)?).unwrap_or(0))
        .map_err(|error| MarketError::Malformed(format!("месяц {value}: {error}")))?;
    Date::from_calendar_date(
        i32::from(parsed(year)?),
        month,
        u8::try_from(parsed(day)?).unwrap_or(0),
    )
    .map_err(|error| MarketError::Malformed(format!("дата {value}: {error}")))
}

/// Разбирает дневной XML в сырой слой, не отбрасывая неизвестные валюты.
pub fn parse_daily_raw(xml: &str) -> Result<Vec<CbrRate>, MarketError> {
    parse_rates(xml, RateContainer::Daily)
}

/// Разбирает дневные курсы и оставляет только известные ядру валюты.
pub fn parse_daily(xml: &str, observed_at: ObservedAt) -> Result<Vec<FxObservation>, MarketError> {
    let raw = parse_daily_raw(xml)?;
    Ok(raw
        .into_iter()
        .filter_map(|rate| {
            // ЦБ публикует больше валют, чем знает доменное ядро.
            // Незнакомые коды пропускаются намеренно, а не считаются
            // ошибкой всего ответа.
            currency_from_iso(&rate.char_code).map(|from| FxObservation {
                from,
                to: CurrencyCode::Rub,
                trade_date: TradeDate(rate.date),
                observed_at,
                nominal: rate.nominal,
                value: Dec::new(rate.value),
                unit_rate: Dec::new(rate.unit_rate),
            })
        })
        .collect())
}

/// Разбирает ряд одной валюты, отбрасывая выходные.
pub fn parse_dynamic(
    xml: &str,
    to: CurrencyCode,
    observed_at: ObservedAt,
) -> Result<Vec<FxObservation>, MarketError> {
    let raw = parse_rates(xml, RateContainer::Dynamic)?;
    Ok(raw
        .into_iter()
        .filter(|rate| !matches!(rate.date.weekday(), Weekday::Saturday | Weekday::Sunday))
        .filter_map(|rate| {
            // В XML_dynamic у Record есть только CBR ID. Известные
            // идентификаторы отображаются в исчерпаемый enum ядра;
            // остальные записи, как и неизвестные CharCode, пропускаются.
            currency_from_cbr_id(&rate.char_code).map(|from| FxObservation {
                from,
                to,
                trade_date: TradeDate(rate.date),
                observed_at,
                nominal: rate.nominal,
                value: Dec::new(rate.value),
                unit_rate: Dec::new(rate.unit_rate),
            })
        })
        .collect())
}

#[derive(Clone, Copy)]
enum RateContainer {
    Daily,
    Dynamic,
}

#[derive(Clone, Copy)]
enum Field {
    CharCode,
    Nominal,
    Value,
    UnitRate,
}

#[derive(Default)]
struct RateBuilder {
    char_code: Option<String>,
    nominal: Option<u32>,
    value: Option<Decimal>,
    unit_rate: Option<Decimal>,
    date: Option<Date>,
}

impl RateBuilder {
    fn finish(self) -> Result<CbrRate, MarketError> {
        let char_code = self
            .char_code
            .filter(|value| !value.is_empty())
            .ok_or_else(|| MarketError::Malformed("у записи ЦБ отсутствует код валюты".into()))?;
        let nominal = self
            .nominal
            .filter(|value| *value > 0)
            .ok_or_else(|| MarketError::Malformed("у записи ЦБ отсутствует номинал".into()))?;
        let value = self
            .value
            .ok_or_else(|| MarketError::Malformed("у записи ЦБ отсутствует Value".into()))?;
        let unit_rate = self
            .unit_rate
            .ok_or_else(|| MarketError::Malformed("у записи ЦБ отсутствует VunitRate".into()))?;
        let date = self
            .date
            .ok_or_else(|| MarketError::Malformed("у записи ЦБ отсутствует дата".into()))?;
        Ok(CbrRate {
            char_code,
            nominal,
            value,
            unit_rate,
            date,
        })
    }

    fn set(&mut self, field: Field, text: &str) -> Result<(), MarketError> {
        match field {
            Field::CharCode => self.char_code = Some(text.to_owned()),
            Field::Nominal => {
                self.nominal =
                    Some(text.parse::<u32>().map_err(|error| {
                        MarketError::Malformed(format!("номинал {text}: {error}"))
                    })?)
            }
            Field::Value => self.value = Some(parse_cbr_decimal(text)?),
            Field::UnitRate => self.unit_rate = Some(parse_cbr_decimal(text)?),
        }
        Ok(())
    }
}

fn parse_rates(xml: &str, container: RateContainer) -> Result<Vec<CbrRate>, MarketError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut rates = Vec::new();
    let mut root_date = None;
    let mut root_id = None;
    let mut current = None;
    let mut field = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = start.name();
                if name.as_ref() == b"ValCurs" {
                    root_date = attribute(&start, b"Date")?
                        .map(|date| parse_cbr_date(&date))
                        .transpose()?;
                    root_id = attribute(&start, b"ID")?;
                } else if is_container(name.as_ref(), container) {
                    if current.is_some() {
                        return Err(MarketError::Malformed("вложенная запись ЦБ".into()));
                    }
                    let mut builder = RateBuilder {
                        char_code: match container {
                            RateContainer::Daily => None,
                            RateContainer::Dynamic => {
                                attribute(&start, b"Id")?.or_else(|| root_id.clone())
                            }
                        },
                        date: match container {
                            RateContainer::Daily => root_date,
                            RateContainer::Dynamic => attribute(&start, b"Date")?
                                .map(|date| parse_cbr_date(&date))
                                .transpose()?,
                        },
                        ..RateBuilder::default()
                    };
                    if matches!(container, RateContainer::Daily) {
                        builder.char_code = attribute(&start, b"CharCode")?;
                    }
                    current = Some(builder);
                } else if current.is_some() {
                    field = field_for(name.as_ref());
                }
            }
            Ok(Event::Text(text)) => {
                if let (Some(builder), Some(field)) = (&mut current, field) {
                    let value = text.decode().map_err(|error| {
                        MarketError::Malformed(format!("текст XML ЦБ: {error}"))
                    })?;
                    builder.set(field, value.trim())?;
                }
            }
            Ok(Event::End(end)) => {
                let name = end.name();
                if is_container(name.as_ref(), container) {
                    let builder = current.take().ok_or_else(|| {
                        MarketError::Malformed("закрыта незаполненная запись ЦБ".into())
                    })?;
                    rates.push(builder.finish()?);
                    field = None;
                } else if field_for(name.as_ref()).is_some() {
                    field = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(MarketError::Malformed(format!("XML ЦБ: {error}")));
            }
        }
    }

    if current.is_some() {
        return Err(MarketError::Malformed("оборванная запись ЦБ".into()));
    }
    if matches!(container, RateContainer::Daily) && root_date.is_none() {
        return Err(MarketError::Malformed(
            "у дневного ответа ЦБ отсутствует Date".into(),
        ));
    }
    Ok(rates)
}

fn is_container(name: &[u8], container: RateContainer) -> bool {
    match container {
        RateContainer::Daily => name == b"Valute",
        RateContainer::Dynamic => name == b"Record",
    }
}

fn field_for(name: &[u8]) -> Option<Field> {
    match name {
        b"CharCode" => Some(Field::CharCode),
        b"Nominal" => Some(Field::Nominal),
        b"Value" => Some(Field::Value),
        b"VunitRate" => Some(Field::UnitRate),
        _ => None,
    }
}

fn attribute(start: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, MarketError> {
    for attribute in start.attributes() {
        let attribute = attribute
            .map_err(|error| MarketError::Malformed(format!("атрибут XML ЦБ: {error}")))?;
        if attribute.key.as_ref() == key {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|error| MarketError::Malformed(format!("атрибут XML ЦБ: {error}")));
        }
    }
    Ok(None)
}

fn currency_from_iso(code: &str) -> Option<CurrencyCode> {
    match code {
        "RUB" => Some(CurrencyCode::Rub),
        "USD" => Some(CurrencyCode::Usd),
        "EUR" => Some(CurrencyCode::Eur),
        "CNY" => Some(CurrencyCode::Cny),
        "XAU" => Some(CurrencyCode::Xau),
        _ => None,
    }
}

fn currency_from_cbr_id(id: &str) -> Option<CurrencyCode> {
    match id {
        "R01235" => Some(CurrencyCode::Usd),
        "R01239" => Some(CurrencyCode::Eur),
        "R01375" => Some(CurrencyCode::Cny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::money::CurrencyCode;
    use time::macros::{date, datetime};

    const DAILY: &[u8] = include_bytes!("../../../../tests/fixtures/market/cbr-xml-daily.xml");
    const DYNAMIC: &[u8] =
        include_bytes!("../../../../tests/fixtures/market/cbr-xml-dynamic-usd.xml");

    fn observed() -> ObservedAt {
        ObservedAt(datetime!(2026-08-26 09:00:00 UTC))
    }

    #[test]
    fn the_response_is_cp1251_and_utf8_decoding_would_fail() {
        // Пролог ответа объявляет windows-1251, и в названиях валют
        // лежат байты, которые UTF-8 не принимает.
        let bytes = DAILY.to_vec();
        assert!(
            core::str::from_utf8(&bytes).is_err(),
            "фикстура перестала быть cp1251 — её подменили"
        );
        let text = decode_cp1251(DAILY);
        assert!(
            text.contains("Австралийский доллар"),
            "декодирование не дало кириллицы"
        );
    }

    #[test]
    fn a_decimal_comma_is_the_source_convention_not_a_typo() {
        assert_eq!(
            parse_cbr_decimal("85,1293").expect("число").to_string(),
            "85.1293"
        );
        assert!(
            parse_cbr_decimal("85.1293").is_err(),
            "точка не является конвенцией ЦБ"
        );
    }

    #[test]
    fn nominal_and_unit_rate_are_both_kept() {
        // Проверяется на СЫРОМ слое, а не на наблюдениях, и это не обход:
        // у всех валют, которые знает ядро (RUB, USD, EUR, CNY), номинал
        // ЦБ равен единице, и различие value/unit_rate на них ненаблюдаемо.
        // Номинал больше единицы есть у иены (100) и лиры (10) — валют,
        // которых в ядре нет. Сырой слой существует именно поэтому:
        // разбор обязан быть проверяем независимо от того, какие валюты
        // система учитывает сегодня.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("разбор");
        let jpy = raw
            .iter()
            .find(|r| r.char_code == "JPY")
            .expect("иена есть в справочнике ЦБ");
        assert_eq!(jpy.nominal, 100, "ЦБ публикует иену за сто единиц");
        assert_ne!(
            jpy.value, jpy.unit_rate,
            "значение за номинал и за единицу совпали — номинал потерян"
        );
    }

    #[test]
    fn a_currency_the_core_does_not_know_is_skipped_not_an_error() {
        // Справочник ЦБ содержит десятки валют, которых система
        // не учитывает. Объявить их ошибкой значило бы уронить разбор
        // всего ответа из-за валюты, которая никому не нужна.
        let text = decode_cp1251(DAILY);
        let raw = parse_daily_raw(&text).expect("разбор");
        let observations = parse_daily(&text, observed()).expect("разбор");
        assert!(
            raw.len() > observations.len(),
            "в справочнике ЦБ больше валют, чем знает ядро: {} против {}",
            raw.len(),
            observations.len()
        );
        assert!(
            raw.iter().any(|r| r.char_code == "JPY"),
            "иена в сыром слое есть"
        );
        assert!(
            observations.iter().all(|o| o.from != CurrencyCode::Rub),
            "рубль не является исходной валютой в котировках ЦБ"
        );
    }

    #[test]
    fn the_series_covers_business_days_only() {
        let text = decode_cp1251(DYNAMIC);
        let series = parse_dynamic(&text, CurrencyCode::Rub, observed()).expect("разбор");
        assert!(!series.is_empty());
        let has_weekend = series.iter().any(|o| {
            matches!(
                o.trade_date.0.weekday(),
                time::Weekday::Saturday | time::Weekday::Sunday
            )
        });
        assert!(
            !has_weekend,
            "в ряду ЦБ выходных нет — курса на воскресенье не существует"
        );
    }

    #[test]
    fn the_source_date_format_is_dotted_not_iso() {
        assert_eq!(
            parse_cbr_date("04.08.2026").expect("дата"),
            date!(2026 - 08 - 04)
        );
        assert!(parse_cbr_date("2026-08-04").is_err());
    }
}
