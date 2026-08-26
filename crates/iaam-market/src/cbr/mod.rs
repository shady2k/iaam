//! ЦБ РФ: курсы валют и ключевая ставка.
//!
//! Два разных интерфейса у одного источника, и это не прихоть:
//! курсы отдаются простыми XML-скриптами, а история ключевой ставки —
//! только SOAP-сервисом. Документированной альтернативы без SOAP
//! и без разбора HTML нет; разбор HTML для источника истины неприемлем,
//! потому что страница меняется без контракта и без версии.

pub mod fx;
pub mod key_rate;

use iaam_http::{Destination, HttpRequest};
use time::Date;

/// Курсы всех валют на дату.
#[must_use]
pub fn daily_request(on: Date) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
        .with_query("date_req", &dotted(on))
}

/// Курс одной валюты за период.
///
/// `cbr_currency_id` — внутренний код ЦБ вида `R01235` (доллар США),
/// а не код ISO: сервис принимает только его.
#[must_use]
pub fn dynamic_request(from: Date, till: Date, cbr_currency_id: &str) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_dynamic.asp")
        .with_query("date_req1", &dotted(from))
        .with_query("date_req2", &dotted(till))
        .with_query("VAL_NM_RQ", cbr_currency_id)
}

/// Дата в формате источника: `DD/MM/YYYY` в запросе.
fn dotted(date: Date) -> String {
    format!(
        "{:02}/{:02}/{}",
        date.day(),
        u8::from(date.month()),
        date.year()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn daily_request_uses_the_cbr_dotted_date_format() {
        let request = daily_request(date!(2026 - 08 - 04));

        assert_eq!(
            request.url(),
            "https://www.cbr.ru/scripts/XML_daily.asp?date%5Freq=04%2F08%2F2026"
        );
    }
}
