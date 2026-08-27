//! MOEX ISS: описание запроса истории торгов.
//!
//! Официальная дневная история, **не свечи**. Свечной эндпойнт
//! (`/candles.json`) существует, но официальной истории не заменяет:
//! это другой источник, и смешивать их в одной серии нельзя.

pub mod bondization;
pub mod dictionary_seed;
pub mod parse;

use iaam_http::{Destination, HttpRequest};
use time::Date;
use time::format_description::well_known::Iso8601;

/// Координата запроса дневной истории.
///
/// Поля собраны в тип, а не разложены по параметрам: четыре из них —
/// строки, и по позиции они взаимозаменяемы. Перестановка `market`
/// с `board` даёт валидный путь к другой площадке, то есть тихо другое
/// наблюдение; имя поля такую ошибку останавливает, а порядок аргументов
/// нет. Порог §17 (`too-many-arguments-threshold = 6`) на семи параметрах
/// говорил ровно об этом.
///
/// `board` полем, а не константой: путь зависит от engine/market/board,
/// и площадка входит в идентичность наблюдения. Зашить `TQBR` значило бы
/// молча решить, что других режимов нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryQuery<'a> {
    /// Торговая система, например `stock`.
    pub engine: &'a str,
    /// Рынок внутри системы, например `shares`.
    pub market: &'a str,
    /// Режим торгов, например `TQBR`.
    pub board: &'a str,
    /// Код бумаги на площадке.
    pub secid: &'a str,
    /// Начало интервала включительно.
    pub from: Date,
    /// Конец интервала включительно.
    pub till: Date,
    /// Смещение страницы: ISS отдаёт историю порциями.
    pub start: u32,
}

/// Запрос дневной истории по бумаге за интервал.
#[must_use]
pub fn history_request(query: HistoryQuery<'_>) -> HttpRequest {
    let HistoryQuery {
        engine,
        market,
        board,
        secid,
        from,
        till,
        start,
    } = query;
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
        let request = history_request(HistoryQuery {
            engine: "stock",
            market: "shares",
            board: "SMAL",
            secid: "SBER",
            from: date!(2026 - 08 - 03),
            till: date!(2026 - 08 - 21),
            start: 0,
        });
        assert!(
            request.url().contains("/boards/SMAL/"),
            "площадка обязана попадать в путь: {}",
            request.url()
        );
    }

    #[test]
    fn the_interval_travels_as_query_parameters() {
        let request = history_request(HistoryQuery {
            engine: "stock",
            market: "shares",
            board: "TQBR",
            secid: "SBER",
            from: date!(2026 - 08 - 03),
            till: date!(2026 - 08 - 21),
            start: 0,
        });
        let url = request.url();
        assert!(url.contains("from=2026%2D08%2D03"), "{url}");
        assert!(url.contains("till=2026%2D08%2D21"), "{url}");
    }
}
