//! MOEX ISS: trading-history request description.
//!
//! Official daily history, **not candles**. The candle endpoint
//! (`/candles.json`) exists but does not replace the official history:
//! it is another source, and the two cannot be mixed in one series.

pub mod bondization;
pub mod description;
pub mod dictionary_seed;
pub mod parse;

use iaam_http::{Destination, HttpRequest};
use time::Date;
use time::format_description::well_known::Iso8601;

/// Daily-history request coordinate.
///
/// Fields are collected in a type rather than split into parameters: four are
/// strings and interchangeable by position. Swapping `market`
/// with `board` yields a valid path to another venue, silently changing the
/// observation; field names stop that mistake, while argument order does not.
/// The §17 threshold (`too-many-arguments-threshold = 6`) for seven parameters
/// exists precisely for this reason.
///
/// `board` is a field, not a constant: the path depends on engine/market/board,
/// and the venue is part of observation identity. Hard-coding `TQBR` would
/// silently decide that no other boards exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryQuery<'a> {
    /// Trading engine, for example `stock`.
    pub engine: &'a str,
    /// Market within the engine, for example `shares`.
    pub market: &'a str,
    /// Trading board, for example `TQBR`.
    pub board: &'a str,
    /// Security code on the venue.
    pub secid: &'a str,
    /// Interval start, inclusive.
    pub from: Date,
    /// Interval end, inclusive.
    pub till: Date,
    /// Page offset: ISS returns history in chunks.
    pub start: u32,
}

/// Request daily history for a security over an interval.
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

/// Source’s actual page-size ceiling.
///
/// This is measured, not a wish: requesting limit 1000 returns
/// 100 rows **without any error**. For the checked issue maturing
/// in 2048, the first page returned 100 coupons ending in 2038 with a
/// closed period chain—the schedule looked complete but was shorter by
/// ten years.
pub const PAGE_LIMIT: u32 = 100;

/// Payment-schedule request coordinate.
///
/// Offset as a field, not a constant: pagination is mandatory, and a request
/// that knows only the first page silently shortens long issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleQuery<'a> {
    /// Security code on the venue.
    pub secid: &'a str,
    /// Page offset. Shared by all three response blocks: on the second
    /// page amortisation and offers are already empty while coupons continue,
    /// so one empty block is not the end of the result.
    pub start: u32,
}

/// Request one page of an issue payment schedule.
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
            "venue must be part of the path: {}",
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
        // A request without an offset returns the first page, and the first
        // page of a long issue is years shorter than the schedule while still
        // appearing closed.
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
        // The source silently cuts the requested limit to one hundred: limit 1000
        // returns 100 rows without any error. Asking above the ceiling
        // means pretending that a page size exists when it does not.
        assert_eq!(PAGE_LIMIT, 100);
    }
}
