//! Parsing the MOEX ISS payment schedule.
//!
//! The response is tabular: `columns` contains names and `data` contains rows. Indices
//! come from `columns` by name rather than hard-coded numbers: ISS adds
//! columns, and positional parsing would eventually read a share as a date.
//!
//! The parser **does not interpret codes**. Principal-return kind and offer-right kind
//! reach the domain unchanged; translation is by dictionary (§2.5). For MOEX
//! the right kind is free Russian text, and a `match` on it would break after
//! the exchange edited its wording.
//!
//! The row nominal field is ignored entirely: it is the security nominal at
//! request time, not the period nominal (§2.11).

use rust_decimal::Decimal;
use serde_json::Value;
use time::Date;
use time::format_description::well_known::Iso8601;

use iaam_core::numeric::decimal::Dec;

use crate::error::MarketError;
use crate::observation::ObservedAt;
use crate::schedule::{CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment};

/// One response page. Pagination is the caller’s responsibility (§2.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondizationPage {
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
    /// Number of rows received across all blocks.
    ///
    /// The caller needs this to distinguish “page empty” from “block empty”:
    /// blocks share an offset, and amortisation ends before coupons.
    pub total_rows: usize,
}

fn block<'a>(root: &'a Value, name: &str) -> Result<(&'a Vec<Value>, Vec<String>), MarketError> {
    let node = root
        .get(name)
        .ok_or_else(|| MarketError::Malformed(format!("missing block {name}")))?;
    let columns = node
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("missing columns in {name}")))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                MarketError::Malformed(format!("column name {name} is not a string"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = node
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError::Malformed(format!("missing data in {name}")))?;
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
        .ok_or_else(|| MarketError::Malformed(format!("{name} is not a string")))?;
    Date::parse(text, &Iso8601::DATE)
        .map(Some)
        .map_err(|_| MarketError::Malformed(format!("{name} is not a date: {text}")))
}

fn required_date(columns: &[String], row: &Value, name: &str) -> Result<Date, MarketError> {
    date_of(columns, row, name)?
        .ok_or_else(|| MarketError::Malformed(format!("{name} is required but empty")))
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
        .map_err(|_| MarketError::Malformed(format!("{name} is not a number: {text}")))
}

fn text_of(columns: &[String], row: &Value, name: &str) -> Option<String> {
    cell(columns, row, name).and_then(|value| value.as_str().map(str::to_owned))
}

fn knowledge<T>(value: Option<T>) -> Knowledge<T> {
    value.map_or(Knowledge::Unknown, Knowledge::Known)
}

/// Parse one `/bondization` response page.
///
/// The caller assigns `observed_at`: trusting the source clock for the knowledge axis
/// would make it forgeable by the response.
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
        // Accrual end and payment date have different meanings. The source gives
        // one value for both; the distinction is preserved because shifting
        // from a weekend moves the latter, not the former.
        let coupon_date = required_date(&coupon_columns, row, "coupondate")?;
        let per_unit = decimal_of(&coupon_columns, row, "value")?;
        let rate_percent = decimal_of(&coupon_columns, row, "valueprc")?;
        let currency = text_of(&coupon_columns, row, "faceunit");
        // Fields are interpreted independently: null amount does not turn the rate into zero.
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
            share_percent: decimal_of(&amort_columns, row, "valueprc")?.ok_or_else(|| {
                MarketError::Malformed("principal return without a share".to_owned())
            })?,
            source_kind: text_of(&amort_columns, row, "data_source").ok_or_else(|| {
                MarketError::Malformed("principal return without a kind".to_owned())
            })?,
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
                .ok_or_else(|| MarketError::Malformed("offer window without a kind".to_owned()))?,
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

    // References were captured by live ISS calls on 2026-08-27 and frozen
    // (`tests/fixtures/MANIFEST.sha256`). A literal constructed
    // from memory tests our model of the source, not the source itself,
    // and can silently diverge from it.
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
            .expect("page parsed")
    }

    #[test]
    fn a_missing_amount_stays_unknown_and_does_not_become_zero() {
        // For the checked floating-rate issue, the past coupon has neither amount nor
        // rate. Zero would understate both cashflow and YTM, plausibly.
        // plausibly.
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
        // Principal currency is not rouble by default. Substituting rouble would
        // add dollars to roubles and produce a plausible number.
        let page = parsed(FOREIGN_FACE);
        let CouponAmount::AmountFixed { currency, .. } = page.coupon_periods[0].amount else {
            panic!(
                "coupon amount is known: {:?}",
                page.coupon_periods[0].amount
            );
        };
        assert_eq!(currency.code(), "USD");
    }

    #[test]
    fn the_source_kind_arrives_uninterpreted() {
        // The code parser does not interpret values: MOEX offer-right kind is
        // free Russian text, and a match on it would break after the exchange edited
        // its wording.
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
        // The row nominal is the security nominal AT REQUEST TIME:
        // for an issue that has undergone amortisation, all rows for all years
        // show the current balance. Treating it as the period principal
        // would recalculate the entire history retroactively.
        let page = parsed(AMORTISED);
        // A return carries a share of initial principal, not an amount
        // derived from the row’s displayed principal.
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
        // The same issue can return windows both with and without a price.
        // An empty price means unknown terms, not a free redemption.
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
        // Reference for the exact trap: the first page is closed by chain and
        // ten years shorter than the real schedule. Only
        // a mismatch between the tail and the last principal return catches it.
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
            "truncated page must yield Incomplete: {outcome:?}"
        );
    }

    #[test]
    fn the_second_page_closes_the_chain_the_first_left_open() {
        // The second page continues the same series: its first period
        // starts where the first page’s last period ended, and together
        // they form the complete schedule. This proves that
        // stopping at the first page was truncation.
        let first = parsed(PAGE_ONE);
        let second = parsed(PAGE_TWO);
        assert_eq!(
            first.coupon_periods.last().expect("first tail").accrual_end,
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
        // A literal rather than a reference intentionally: MOEX does not return this row
        // in any checked issue—it fills `value`
        // and `valueprc` together, or fills neither. The state
        // “rate known, amount not yet determined” is defined by the spec (§2.3) and
        // is preserved by the schema, so parsing must construct it rather than
        // collapse it into `Undetermined`: collapsing would lose the
        // known floating rate.
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
                "rate is known, amount is not: {:?}",
                page.coupon_periods[0].amount
            );
        };
        assert_eq!(rate_percent.inner().to_string(), "6.9");
    }

    #[test]
    fn the_row_count_covers_all_three_blocks_together() {
        // Row count is the only way the caller distinguishes
        // “page empty” from “one block empty”. Counting one block would
        // stop pagination when amortisation ends,
        // truncating coupons.
        let page = parsed(OFFERS);
        assert_eq!(page.coupon_periods.len(), 40);
        assert_eq!(page.principal_repayments.len(), 1);
        assert_eq!(page.offer_windows.len(), 8);
        assert_eq!(page.total_rows, 49);
    }
}
