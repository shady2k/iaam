//! Bond payment schedule synchronisation (§2.10 of the E3.4 spec).
//!
//! There are three differences from `sync_market`, each addressing a specific
//! pitfall:
//!
//! 1. **Pagination.** `sync_market` fetches one page at offset zero.
//!    Here the offset increases while at least one block returns rows: the source
//!    silently limits a page to one hundred rows, and the first request for a long issue
//!    returns a closed schedule ten years shorter than the real one.
//! 2. **Dictionary-based code translation.** Nominal repayment type, offer right type
//!    and currency codes are translated by reading the dictionary. An unknown code —
//!    causes failure naming the code, rather than skipping the row: a skipped row
//!    silently shortens the schedule.
//! 3. **Structural validation.** Completeness comprises three independent assertions,
//!    and «the source was read to the end» does not constitute completeness.
//!
//! An invariant violation does **not** cancel writing the snapshot: the snapshot is what
//! the source actually sent, and deleting it would mean losing
//! evidence. What is invalidated is the schedule's suitability for calculation.

use iaam_core::ids::InstrumentId;
use iaam_market::moex::bondization::parse_bondization_page;
use iaam_market::moex::description::{parse_description, terms_request};
use iaam_market::moex::{PAGE_LIMIT, ScheduleQuery, schedule_request};
use iaam_market::observation::ObservedAt;
use iaam_market::schedule::completeness::{Completeness, validate_moex_profile};
use iaam_market::schedule::{
    CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment,
};
use iaam_store::market::MarketStore;
use iaam_store::schedule::{
    CouponPeriodRow, IssueTermsRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::AppError;
use crate::ports::OutboundHttp;

/// Schedule source identifier.
pub const SOURCE_ID: &str = "moex-iss";

/// Maximum number of pages.
///
/// A safeguard, not an expectation: an issue with monthly coupons over
/// thirty years has four pages. Exiting on the counter causes failure with a reason,
/// rather than a silent return: a silent return would be the same truncation, only
/// done by us.
const MAX_PAGES: u32 = 100;

/// What to synchronise.
#[derive(Debug, Clone)]
pub struct ScheduleSyncRequest {
    pub instrument: InstrumentId,
    pub secid: String,
}

/// Observable run state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSyncResult {
    pub snapshot_id: String,
    /// Whether a new snapshot was written. `false` means the content matched
    /// the previous snapshot, and this is not an error but an event in the run trace.
    pub written: bool,
    pub pages_seen: Vec<u32>,
    pub completeness: Completeness,
}

fn invalid(field: &str, expected: &str, actual: &str) -> AppError {
    AppError::Invalid {
        field: field.to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    }
}

/// Synchronise the payment schedule of one issue.
pub async fn sync_schedule(
    store: &mut MarketStore,
    transport: &dyn OutboundHttp,
    request: ScheduleSyncRequest,
) -> Result<ScheduleSyncResult, AppError> {
    let observed_at = ObservedAt(OffsetDateTime::now_utc());

    let mut coupon_periods: Vec<CouponPeriod> = Vec::new();
    let mut principal_repayments: Vec<PrincipalRepayment> = Vec::new();
    let mut offer_windows: Vec<OfferWindow> = Vec::new();
    let mut pages_seen: Vec<u32> = Vec::new();

    for page_index in 0..MAX_PAGES {
        let start = page_index * PAGE_LIMIT;
        let response = transport
            .send(schedule_request(ScheduleQuery {
                secid: &request.secid,
                start,
            }))
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(invalid(
                "status",
                "successful source response",
                &response.status.to_string(),
            ));
        }
        pages_seen.push(start);
        let page = parse_bondization_page(&response.body, observed_at)
            .map_err(|error| invalid("body", "parseable schedule", &error.to_string()))?;
        // The result set ends only when a page is empty in ALL blocks at once.
        // The offset is shared, and amortisations end before coupons.
        if page.total_rows == 0 {
            break;
        }
        coupon_periods.extend(page.coupon_periods);
        principal_repayments.extend(page.principal_repayments);
        offer_windows.extend(page.offer_windows);

        if page_index + 1 == MAX_PAGES {
            return Err(invalid(
                "pages",
                "schedule shorter than the page limit",
                &MAX_PAGES.to_string(),
            ));
        }
    }

    let repayment_kinds = store
        .market_source_codes(SOURCE_ID, "principal_repayment_kind")
        .map_err(|error| {
            invalid(
                "dictionary",
                "repayment type dictionary",
                &error.to_string(),
            )
        })?;
    let offer_kinds = store
        .market_source_codes(SOURCE_ID, "offer_kind")
        .map_err(|error| invalid("dictionary", "offer type dictionary", &error.to_string()))?;
    let currencies = store
        .market_source_codes(SOURCE_ID, "currency")
        .map_err(|error| invalid("dictionary", "currency dictionary", &error.to_string()))?;

    // An unknown code causes failure that names it explicitly. Skipping the row
    // would silently shorten the schedule, while «Other» would indicate a deliberate decision
    // not to parse it — no such decision was made.
    for repayment in &principal_repayments {
        if !repayment_kinds.contains_key(&repayment.source_kind) {
            return Err(invalid(
                "principal_repayment_kind",
                "code known to the source dictionary",
                &repayment.source_kind,
            ));
        }
    }
    for window in &offer_windows {
        if !offer_kinds.contains_key(&window.source_kind) {
            return Err(invalid(
                "offer_kind",
                "code known to the source dictionary",
                &window.source_kind,
            ));
        }
    }
    for period in &coupon_periods {
        if let CouponAmount::AmountFixed { currency, .. } = &period.amount
            && !currencies.contains_key((*currency).code())
        {
            return Err(invalid(
                "currency",
                "currency code known to the source dictionary",
                (*currency).code(),
            ));
        }
    }

    let completeness = validate_moex_profile(&coupon_periods, &principal_repayments);

    let coupon_rows = coupon_periods.iter().map(coupon_row).collect::<Vec<_>>();
    let repayment_rows = principal_repayments
        .iter()
        .map(repayment_row)
        .collect::<Vec<_>>();
    let window_rows = offer_windows.iter().map(window_row).collect::<Vec<_>>();

    let header = ScheduleSnapshotRow {
        instrument_id: request.instrument.inner().to_string(),
        source_id: SOURCE_ID.to_owned(),
        observed_at: observed_at
            .0
            .format(&Rfc3339)
            .map_err(|error| invalid("observed_at", "RFC 3339", &error.to_string()))?,
        content_hash: content_hash(&coupon_rows, &repayment_rows, &window_rows),
    };
    let outcome = store
        .record_schedule_snapshot(&header, &coupon_rows, &repayment_rows, &window_rows)
        .map_err(|error| invalid("snapshot", "snapshot to be written", &error.to_string()))?;

    let (validated, reason) = match &completeness {
        Completeness::Validated => (true, None),
        Completeness::Incomplete { reason } => (false, Some(reason.clone())),
        // The issue is outside the profile's scope: the invariants do not
        // apply, so they cannot be declared satisfied.
        Completeness::Unknown => (false, Some("issue outside the source profile".to_owned())),
    };
    store
        .record_schedule_completeness(
            &outcome.snapshot_id,
            true,
            validated,
            reason.as_deref(),
            &pages_seen,
        )
        .map_err(|error| {
            invalid(
                "completeness",
                "completeness to be recorded",
                &error.to_string(),
            )
        })?;

    Ok(ScheduleSyncResult {
        snapshot_id: outcome.snapshot_id,
        written: outcome.written,
        pages_seen,
        completeness,
    })
}

/// Synchronise issue terms.
///
/// A separate scenario rather than a schedule synchronisation step: issue terms have their own
/// endpoint, effective axis (`effective_from`) and append-only
/// table. Combining them would mean recording a new observation of the issue terms
/// every time the schedule changed, and vice versa.
pub async fn sync_issue_terms(
    store: &mut MarketStore,
    transport: &dyn OutboundHttp,
    instrument: InstrumentId,
    secid: &str,
) -> Result<(), AppError> {
    let observed_at = ObservedAt(OffsetDateTime::now_utc());
    let response = transport.send(terms_request(secid)).await?;
    if !(200..300).contains(&response.status) {
        return Err(invalid(
            "status",
            "successful source response",
            &response.status.to_string(),
        ));
    }
    let terms = parse_description(&response.body, instrument, observed_at)
        .map_err(|error| invalid("body", "description to be parsed", &error.to_string()))?;

    // The currency code is stored as supplied by the source, but the dictionary must
    // know it: an unknown code reaching the database would become a second currency
    // alongside the rouble, and the positions would silently diverge.
    if let Knowledge::Known(code) = &terms.face_currency_code {
        let currencies = store
            .market_source_codes(SOURCE_ID, "currency")
            .map_err(|error| invalid("dictionary", "currency dictionary", &error.to_string()))?;
        if !currencies.contains_key(code) {
            return Err(invalid(
                "currency",
                "currency code known to the source dictionary",
                code,
            ));
        }
    }

    store
        .record_issue_terms(&IssueTermsRow {
            instrument_id: instrument.inner().to_string(),
            source_id: SOURCE_ID.to_owned(),
            observed_at: observed_at
                .0
                .format(&Rfc3339)
                .map_err(|error| invalid("observed_at", "RFC 3339", &error.to_string()))?,
            // Unknown reaches the database as NULL. A default value
            // here would be plausible but incorrect accrued coupon interest.
            effective_from: terms.effective_from.known().map(ToString::to_string),
            maturity_date: terms.maturity_date.known().map(ToString::to_string),
            initial_face_value: terms
                .initial_face_value
                .known()
                .map(|value| value.inner().to_string()),
            face_currency_code: terms.face_currency_code.known().cloned(),
            coupon_periods_per_year: terms
                .coupon_periods_per_year
                .known()
                .map(|value| i64::from(*value)),
            day_count: terms.day_count.known().cloned(),
            calendar: terms.calendar.known().cloned(),
            default_declared: terms.default_flags.declared,
            default_technical: terms.default_flags.technical,
        })
        .map_err(|error| {
            invalid(
                "issue_terms",
                "issue terms to be recorded",
                &error.to_string(),
            )
        })?;
    Ok(())
}

fn coupon_row(period: &CouponPeriod) -> CouponPeriodRow {
    let (status, per_unit, currency, rate) = match &period.amount {
        CouponAmount::AmountFixed { per_unit, currency } => (
            "amount_fixed",
            Some(per_unit.inner().to_string()),
            Some(currency.code().to_owned()),
            None,
        ),
        CouponAmount::RateFixedAmountUndetermined { rate_percent } => (
            "rate_fixed_amount_undetermined",
            None,
            None,
            Some(rate_percent.inner().to_string()),
        ),
        CouponAmount::Undetermined => ("undetermined", None, None, None),
    };
    CouponPeriodRow {
        period_start: period.period_start.to_string(),
        accrual_end: period.accrual_end.to_string(),
        payment_date: period.payment_date.to_string(),
        record_date: period.record_date.known().map(ToString::to_string),
        amount_status: status.to_owned(),
        amount_per_unit: per_unit,
        amount_currency: currency,
        rate_percent: rate,
        source_entry_id: period.source_entry_id.clone(),
    }
}

fn repayment_row(repayment: &PrincipalRepayment) -> PrincipalRepaymentRow {
    PrincipalRepaymentRow {
        repayment_date: repayment.repayment_date.to_string(),
        share_percent: repayment.share_percent.inner().to_string(),
        source_kind: repayment.source_kind.clone(),
        source_entry_id: repayment.source_entry_id.clone(),
    }
}

fn window_row(window: &OfferWindow) -> OfferWindowRow {
    OfferWindowRow {
        execution_date: window.execution_date.to_string(),
        submission_start: window.submission_start.known().map(ToString::to_string),
        submission_end: window.submission_end.known().map(ToString::to_string),
        price_percent: window
            .price_percent
            .known()
            .map(|value| value.inner().to_string()),
        agent: window.agent.known().cloned(),
        source_kind: window.source_kind.clone(),
        source_entry_id: window.source_entry_id.clone(),
    }
}

/// Snapshot content hash.
///
/// Calculated from table rows, not the response body: the body changes because of
/// fields outside the domain (current face value in each row,
/// rouble equivalent, number of days to maturity), and hashing it would mark
/// an unchanged schedule as changed every day.
fn content_hash(
    coupons: &[CouponPeriodRow],
    repayments: &[PrincipalRepaymentRow],
    windows: &[OfferWindowRow],
) -> String {
    let mut hasher = Sha256::new();
    for row in coupons {
        hasher.update(
            format!(
                "c|{}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}\n",
                row.period_start,
                row.accrual_end,
                row.payment_date,
                row.record_date,
                row.amount_status,
                row.amount_per_unit,
                row.amount_currency,
                row.rate_percent
            )
            .as_bytes(),
        );
    }
    for row in repayments {
        hasher.update(
            format!(
                "p|{}|{}|{}\n",
                row.repayment_date, row.share_percent, row.source_kind
            )
            .as_bytes(),
        );
    }
    for row in windows {
        hasher.update(
            format!(
                "o|{}|{:?}|{:?}|{:?}|{:?}|{}\n",
                row.execution_date,
                row.submission_start,
                row.submission_end,
                row.price_percent,
                row.agent,
                row.source_kind
            )
            .as_bytes(),
        );
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
