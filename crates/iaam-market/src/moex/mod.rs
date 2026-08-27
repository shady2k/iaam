//! MOEX ISS: описание запроса истории торгов.
//!
//! Официальная дневная история, **не свечи**. Свечной эндпойнт
//! (`/candles.json`) существует, но официальной истории не заменяет:
//! это другой источник, и смешивать их в одной серии нельзя.

pub mod bondization;
pub mod description;
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

/// Фактический потолок страницы у источника.
///
/// Не пожелание, а измеренная величина: запрошенный лимит 1000 отдаёт
/// 100 строк **без всякой ошибки**. У проверенного выпуска с погашением
/// в 2048 году первая страница вернула 100 купонов с хвостом 2038 и
/// замкнутой цепью периодов — график выглядел полным и был короче на
/// десять лет.
pub const PAGE_LIMIT: u32 = 100;

/// Координата запроса графика выплат.
///
/// Смещение полем, а не константой: пагинация обязательна, и запрос,
/// умеющий только первую страницу, молча укорачивает длинные выпуски.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleQuery<'a> {
    /// Код бумаги на площадке.
    pub secid: &'a str,
    /// Смещение страницы. Общее на все три блока ответа: на второй
    /// странице амортизации и оферты уже пусты, а купоны продолжаются,
    /// поэтому пустота одного блока концом выборки не является.
    pub start: u32,
}

/// Запрос одной страницы графика выплат по бумаге.
#[must_use]
pub fn schedule_request(query: ScheduleQuery<'_>) -> HttpRequest {
    let ScheduleQuery { secid, start } = query;
    let path = format!("/iss/securities/{secid}/bondization.json");
    HttpRequest::get(Destination::MoexIss, &path)
        .with_query("limit", &PAGE_LIMIT.to_string())
        .with_query("start", &start.to_string())
        .with_query("iss.meta", "off")
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

    #[test]
    fn the_schedule_request_carries_an_explicit_offset() {
        // Запрос без смещения возвращает первую страницу, а первая
        // страница у длинного выпуска короче графика на годы — и при этом
        // выглядит замкнутой.
        let request = schedule_request(ScheduleQuery {
            secid: "SU46020RMFS2",
            start: 100,
        });
        let url = request.url();
        assert!(
            url.contains("/securities/SU46020RMFS2/bondization.json"),
            "{url}"
        );
        assert!(url.contains("start=100"), "{url}");
    }

    #[test]
    fn the_page_limit_is_the_actual_ceiling_not_a_wish() {
        // Источник молча режет запрошенный лимит до сотни: лимит 1000
        // отдаёт 100 строк без всякой ошибки. Просить больше потолка
        // значит договориться с собой о размере страницы, которого нет.
        assert_eq!(PAGE_LIMIT, 100);
    }
}
