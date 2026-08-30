//! Parsing an ISS response.
//!
//! The response is tabular: `columns` is an array of names and `data` is an array
//! of rows. Column indices come from `columns` by name, not
//! hard-coded numbers: ISS adds columns, and positional parsing
//! will eventually read volume as price.

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

/// ISS price columns and their meanings.
///
/// All six are equal candidates: choosing among them is valuation policy (E3.3).
const PRICE_COLUMNS: [(&str, PriceKind); 6] = [
    ("CLOSE", PriceKind::Close),
    ("LEGALCLOSEPRICE", PriceKind::LegalClose),
    ("WAPRICE", PriceKind::WeightedAverage),
    ("MARKETPRICE2", PriceKind::MarketPrice2),
    ("MARKETPRICE3", PriceKind::MarketPrice3),
    ("ADMITTEDQUOTE", PriceKind::AdmittedQuote),
];

/// Map a source currency code to a domain code.
///
/// `SUR` — the Soviet rouble code from an old standard that the exchange
/// never changed. Without this mapping, parsing either fails on every
/// rouble-denominated security or creates a second currency beside the rouble,
/// splitting positions across two currencies with one meaning.
pub(crate) fn currency_of(code: &str) -> Result<CurrencyCode, MarketError> {
    match code {
        "SUR" | "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        other => Err(MarketError::UnknownCurrency(other.to_owned())),
    }
}

/// ISS segment from which the quote row was taken.
///
/// These are the same `engine` and `market` used to build the request path
/// (`super::history_request`), so the adapter knows the basis in advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketSegment<'a> {
    pub engine: &'a str,
    pub market: &'a str,
}

impl MarketSegment<'_> {
    /// Quotation basis and the evidence from which it was derived.
    ///
    /// The table describes request-path pairs, not instrument type.
    /// An unknown pair remains unknown so a guess is not presented
    /// as a proven monetary value.
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

/// Basis derived from the complete ISS segment.
///
/// `None` means no evidence or evidence from another source.
/// An unknown but well-formed ISS segment returns
/// `Some(Unknown)`: the segment format is proven, but the segment table does not yet
/// know it.
#[must_use]
pub fn quotation_basis_from_evidence(evidence: &str) -> Option<QuotationBasis> {
    let path = evidence.strip_prefix("iss:engines/")?;
    let (engine, market) = path.split_once("/markets/")?;
    if engine.is_empty() || market.is_empty() || engine.contains('/') || market.contains('/') {
        return None;
    }
    Some(MarketSegment { engine, market }.quotation_basis().0)
}

/// Compare the recorded basis with evidence from the row.
///
/// Return the effective basis and a contradiction flag. Missing
/// evidence is not a contradiction: in that case a known
/// recorded basis is simply reduced to `Unknown`.
#[must_use]
pub fn reconcile_quotation_basis(
    recorded: QuotationBasis,
    evidence: &str,
) -> (QuotationBasis, bool) {
    match quotation_basis_from_evidence(evidence) {
        Some(inferred) if inferred != QuotationBasis::Unknown => {
            if inferred == recorded {
                (recorded, false)
            } else {
                (QuotationBasis::Unknown, true)
            }
        }
        _ => (QuotationBasis::Unknown, false),
    }
}

/// Parse a history page into observations.
///
/// `observed_at` comes **from outside**: the ISS response contains no
/// observation time, so the system must assign it.
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
        .ok_or_else(|| MarketError::Malformed("missing block history".to_owned()))?;
    let names = column_names(block)?;
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("missing history.data".to_owned()))?;

    ensure_page_is_whole(&root, rows.len())?;

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("history.data row is not an array".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("row is missing TRADEDATE".to_owned()))?,
        )?);
        let currency = currency_of(
            get("CURRENCYID")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("row is missing CURRENCYID".to_owned()))?,
        )?;
        let venue = Venue {
            board: get("BOARDID")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("row is missing BOARDID".to_owned()))?
                .to_owned(),
            session: get("TRADINGSESSION").and_then(Value::as_i64).unwrap_or(0),
        };
        for (column, kind) in PRICE_COLUMNS {
            // An empty observation column creates no observation: a missing
            // value is an Option, not zero (§4.9). Zero as a price
            // would mean “the security is worthless”.
            let Some(value) = get(column) else {
                continue;
            };
            let Some(number) = value.as_number() else {
                if value.is_null() {
                    continue;
                }
                return Err(MarketError::Malformed(format!(
                    "column {column} is not a number"
                )));
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
                // Daily history gives a closing price, not an executable bid.
                // Marking it executable would present an indicative value
                // as an exit price (§5.1, §5.3).
                executability: Executability::IndicativePreviousClose,
            });
        }
    }
    Ok(observations)
}

/// Parse accrued-interest observations from the same history page.
///
/// A separate function rather than a branch inside `parse_history`: values have different
/// dimensions (a principal percentage versus money) and different fates—
/// mixing them in one loop would eventually record one in place of the other.
pub fn parse_accrued_interest(
    body: &str,
    instrument: InstrumentId,
    observed_at: ObservedAt,
) -> Result<Vec<AccruedInterestObservation>, MarketError> {
    let root: Value =
        serde_json::from_str(body).map_err(|error| MarketError::Malformed(error.to_string()))?;
    let block = root
        .get("history")
        .ok_or_else(|| MarketError::Malformed("missing block history".to_owned()))?;
    let names = column_names(block)?;
    let rows = block
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed("missing history.data".to_owned()))?;
    ensure_page_is_whole(&root, rows.len())?;
    // No column at all means this is not a bond segment, not a failure.
    if index_of(&names, "ACCINT").is_none() {
        return Ok(Vec::new());
    }

    let mut observations = Vec::new();
    for row in rows {
        let row = row
            .as_array()
            .ok_or_else(|| MarketError::Malformed("history.data row is not an array".to_owned()))?;
        let get = |name: &str| index_of(&names, name).and_then(|i| row.get(i));
        // An empty observation value creates no observation: zero accrued interest would mean
        // the start of a coupon period, not an absence of trading.
        let Some(value) = get("ACCINT").and_then(Value::as_number) else {
            continue;
        };
        let amount = value
            .to_string()
            .parse::<Decimal>()
            .map_err(|error| MarketError::Malformed(error.to_string()))?;
        // Accrued-interest currency is the principal currency (FACEUNIT), not the venue’s
        // settlement currency (CURRENCYID). They differ in one row.
        let currency =
            currency_of(get("FACEUNIT").and_then(Value::as_str).ok_or_else(|| {
                MarketError::Malformed("ACCINT row is missing FACEUNIT".to_owned())
            })?)?;
        let trade_date = TradeDate(parse_date(
            get("TRADEDATE")
                .and_then(Value::as_str)
                .ok_or_else(|| MarketError::Malformed("row is missing TRADEDATE".to_owned()))?,
        )?);
        observations.push(AccruedInterestObservation {
            instrument,
            venue: Venue {
                board: get("BOARDID")
                    .and_then(Value::as_str)
                    .ok_or_else(|| MarketError::Malformed("row is missing BOARDID".to_owned()))?
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

/// The page arrived complete.
///
/// The ISS cursor provides `INDEX`, `TOTAL`, and `PAGESIZE`. A partial page
/// mistaken for a complete page creates a gap in the series that cannot later be
/// distinguished from a non-trading day—silently corrupting history.
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
        .ok_or_else(|| MarketError::Malformed("missing columns".to_owned()))?
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_owned)
                .ok_or_else(|| MarketError::Malformed("column name is not a string".to_owned()))
        })
        .collect()
}

fn index_of(names: &[String], name: &str) -> Option<usize> {
    names.iter().position(|candidate| candidate == name)
}

fn parse_date(value: &str) -> Result<Date, MarketError> {
    Date::parse(value, &Iso8601::DATE)
        .map_err(|error| MarketError::Malformed(format!("date {value}: {error}")))
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
        // SUR — the Soviet rouble code from an old standard that the exchange
        // never changed. A parser unaware of this either fails or creates
        // a second currency beside the rouble.
        assert_eq!(currency_of("SUR").expect("rouble"), CurrencyCode::Rub);
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
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("parsing fixture");
        let first_day: Vec<_> = observations
            .iter()
            .filter(|o| o.trade_date == TradeDate(date!(2026 - 08 - 03)))
            .collect();
        // In the fixture the first row has empty ADMITTEDQUOTE; the other five
        // columns are populated.
        assert_eq!(
            first_day.len(),
            5,
            "expected five observations for the day, got {}",
            first_day.len()
        );
        assert!(
            !first_day.iter().any(|o| o.kind == PriceKind::AdmittedQuote),
            "an empty column must not create an observation"
        );
    }

    #[test]
    fn the_venue_and_session_travel_with_the_observation() {
        let observations =
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("parsing fixture");
        let first = observations.first().expect("at least one observation");
        assert_eq!(first.venue.board, "TQBR");
        assert_eq!(first.venue.session, 3);
        assert_eq!(first.currency, CurrencyCode::Rub);
    }

    #[test]
    fn the_knowledge_axis_comes_from_the_caller_not_the_response() {
        // The ISS response contains no observation time. It is assigned
        // by the system: trusting the source would make the knowledge axis
        // forgeable by the response.
        let observations =
            parse_history(FIXTURE, instrument(), observed(), SHARES).expect("parsing fixture");
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
        // An unknown market has an unknown quotation basis, not money per unit by default.
        let segment = MarketSegment {
            engine: "currency",
            market: "selt",
        };
        assert_eq!(segment.quotation_basis().0, QuotationBasis::Unknown);
    }

    #[test]
    fn the_basis_comes_from_the_segment_not_from_the_response_body() {
        // The market supplies the basis, not the response row contents.
        let instrument = InstrumentId::new_random();
        let observed_at = ObservedAt(datetime!(2026-08-21 19:00:00 UTC));
        let as_shares = parse_history(FIXTURE, instrument, observed_at, SHARES).unwrap();
        let as_bonds = parse_history(FIXTURE, instrument, observed_at, BONDS).unwrap();

        assert_eq!(as_shares[0].basis, QuotationBasis::MoneyPerUnit);
        assert_eq!(as_bonds[0].basis, QuotationBasis::PercentOfRemainingFace);
        assert_eq!(as_shares[0].price, as_bonds[0].price, "price is unchanged");
    }
    #[test]
    fn matching_known_basis_is_proven() {
        let (basis, contradicts) = reconcile_quotation_basis(
            QuotationBasis::PercentOfRemainingFace,
            "iss:engines/stock/markets/bonds",
        );
        assert_eq!(basis, QuotationBasis::PercentOfRemainingFace);
        assert!(!contradicts);
    }

    #[test]
    fn known_contradictory_basis_becomes_unknown() {
        let (basis, contradicts) = reconcile_quotation_basis(
            QuotationBasis::MoneyPerUnit,
            "iss:engines/stock/markets/bonds",
        );
        assert_eq!(basis, QuotationBasis::Unknown);
        assert!(contradicts);

        let (basis, contradicts) =
            reconcile_quotation_basis(QuotationBasis::Unknown, "iss:engines/stock/markets/bonds");
        assert_eq!(basis, QuotationBasis::Unknown);
        assert!(contradicts);
    }

    #[test]
    fn unproven_unknown_basis_is_accepted() {
        for evidence in ["", "test:market", "iss:engines/stock/markets/futures"] {
            let (basis, contradicts) = reconcile_quotation_basis(QuotationBasis::Unknown, evidence);
            assert_eq!(basis, QuotationBasis::Unknown, "evidence: {evidence}");
            assert!(!contradicts, "evidence: {evidence}");
        }
    }

    #[test]
    fn known_basis_without_evidence_is_cleared() {
        for evidence in ["", "test:market", "iss:engines/stock/markets/futures"] {
            let (basis, contradicts) =
                reconcile_quotation_basis(QuotationBasis::MoneyPerUnit, evidence);
            assert_eq!(basis, QuotationBasis::Unknown, "evidence: {evidence}");
            assert!(!contradicts, "evidence: {evidence}");
        }
    }

    #[test]
    fn reverse_parsing_returns_basis_for_known_pair() {
        for (engine, market) in [("stock", "bonds"), ("stock", "shares")] {
            let segment = MarketSegment { engine, market };
            let (basis, evidence) = segment.quotation_basis();
            assert_eq!(quotation_basis_from_evidence(&evidence), Some(basis));
        }
    }
    #[test]
    fn reverse_parsing_rejects_empty_engine() {
        assert_eq!(
            quotation_basis_from_evidence("iss:engines//markets/shares"),
            None
        );
    }

    #[test]
    fn reverse_parsing_rejects_empty_market() {
        assert_eq!(
            quotation_basis_from_evidence("iss:engines/stock/markets/"),
            None
        );
    }

    #[test]
    fn reverse_parsing_rejects_slash_in_engine() {
        assert_eq!(
            quotation_basis_from_evidence("iss:engines/stock/extra/markets/shares"),
            None
        );
    }

    #[test]
    fn reverse_parsing_rejects_slash_in_market() {
        assert_eq!(
            quotation_basis_from_evidence("iss:engines/stock/markets/shares/extra"),
            None
        );
    }

    #[test]
    fn unknown_segment_differs_from_incomplete_evidence() {
        assert_eq!(
            quotation_basis_from_evidence("iss:engines/other/markets/futures"),
            Some(QuotationBasis::Unknown)
        );
        assert_eq!(
            quotation_basis_from_evidence("iss:engines/other/futures"),
            None
        );
    }

    #[test]
    fn accrued_interest_takes_its_currency_from_face_unit_not_from_currency_id() {
        // In one row the source names the currency twice, differently:
        // CURRENCYID=SUR and FACEUNIT=RUB. Accrued interest is in principal currency.
        let observations = parse_accrued_interest(
            BOND_HISTORY,
            InstrumentId::new_random(),
            ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
        )
        .unwrap();
        assert_eq!(
            observations.len(),
            1,
            "a row with null observation creates none"
        );
        assert_eq!(observations[0].per_unit.currency(), CurrencyCode::Rub);
        assert_eq!(
            observations[0].per_unit.value(),
            Dec::new(Decimal::from_str_exact("15.17").unwrap())
        );
    }
    #[test]
    fn accrued_interest_rejects_a_truncated_page() {
        let body = r#"{"history":{
            "columns":["BOARDID","TRADEDATE","ACCINT","FACEUNIT","TRADINGSESSION"],
            "data":[["TQOB","2026-08-20",15.17,"RUB",3]]},
            "history.cursor":{
                "columns":["INDEX","TOTAL","PAGESIZE"],
                "data":[[0,2,100]]}}"#;
        assert!(matches!(
            parse_accrued_interest(
                body,
                InstrumentId::new_random(),
                ObservedAt(datetime!(2026-08-27 12:00:00 UTC)),
            ),
            Err(MarketError::Truncated { got: 1, total: 2 })
        ));
    }

    #[test]
    fn a_response_without_the_column_yields_nothing_rather_than_failing() {
        // An equity response has no ACCINT column at all. Refusing here
        // would break synchronisation for all non-bonds.
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
