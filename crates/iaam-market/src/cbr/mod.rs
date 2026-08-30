//! CBR: exchange rates and key rate.
//!
//! One source exposes two different interfaces, and that is not a whim:
//! rates come from simple XML scripts, while key-rate history is available
//! only through SOAP. There is no documented alternative without SOAP and
//! without parsing HTML; HTML parsing is unacceptable for a source of truth
//! because the page changes without a contract or version.

pub mod fx;
pub mod key_rate;

use iaam_http::{Destination, HttpRequest};
use time::Date;

/// Rates for all currencies on a date.
#[must_use]
pub fn daily_request(on: Date) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_daily.asp")
        .with_query("date_req", &dotted(on))
}

/// Rate for one currency over a period.
///
/// `cbr_currency_id` is the CBR's internal code, such as `R01235` (US dollar),
/// not an ISO code: the service accepts only the former.
#[must_use]
pub fn dynamic_request(from: Date, till: Date, cbr_currency_id: &str) -> HttpRequest {
    HttpRequest::get(Destination::CbrScripts, "/scripts/XML_dynamic.asp")
        .with_query("date_req1", &dotted(from))
        .with_query("date_req2", &dotted(till))
        .with_query("VAL_NM_RQ", cbr_currency_id)
}

/// Date in the source format: `DD/MM/YYYY` in the request.
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
