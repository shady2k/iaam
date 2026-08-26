//! MOEX ISS: описание запроса истории торгов.
//!
//! Официальная дневная история, **не свечи**. Свечной эндпойнт
//! (`/candles.json`) существует, но официальной истории не заменяет:
//! это другой источник, и смешивать их в одной серии нельзя.

pub mod parse;

use iaam_http::{Destination, HttpRequest};
use time::Date;
use time::format_description::well_known::Iso8601;

/// Запрос дневной истории по бумаге за интервал.
///
/// `board` параметром, а не константой: путь зависит от
/// engine/market/board, и площадка входит в идентичность наблюдения.
/// Зашить `TQBR` значило бы молча решить, что других режимов нет.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn history_request(
    engine: &str,
    market: &str,
    board: &str,
    secid: &str,
    from: Date,
    till: Date,
    start: u32,
) -> HttpRequest {
    let path = format!(
        "/iss/history/engines/{engine}/markets/{market}/boards/{board}/securities/{secid}.json"
    );
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("from", &iso(from))
        .with_query("till", &iso(till))
        .with_query("start", &start.to_string())
}

fn iso(date: Date) -> String {
    date.format(&Iso8601::DATE).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn the_board_is_part_of_the_path_not_a_constant() {
        let request = history_request(
            "stock",
            "shares",
            "SMAL",
            "SBER",
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 21),
            0,
        );
        assert!(
            request.url().contains("/boards/SMAL/"),
            "площадка обязана попадать в путь: {}",
            request.url()
        );
    }

    #[test]
    fn the_interval_travels_as_query_parameters() {
        let request = history_request(
            "stock",
            "shares",
            "TQBR",
            "SBER",
            date!(2026 - 08 - 03),
            date!(2026 - 08 - 21),
            0,
        );
        let url = request.url();
        assert!(url.contains("from=2026%2D08%2D03"), "{url}");
        assert!(url.contains("till=2026%2D08%2D21"), "{url}");
    }
}
