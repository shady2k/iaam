//! Синхронизация графика выплат облигации (§2.10 спеки E3.4).
//!
//! Три отличия от `sync_market`, и каждое существует ради конкретной
//! ловушки:
//!
//! 1. **Пагинация.** `sync_market` берёт одну страницу со смещением ноль.
//!    Здесь смещение растёт, пока хоть один блок отдаёт строки: источник
//!    молча режет страницу до сотни, и первый запрос у длинного выпуска
//!    возвращает замкнутый график, короче настоящего на десять лет.
//! 2. **Перевод кодов словарём.** Коды вида возврата номинала, вида права
//!    по оферте и валюты переводятся чтением словаря. Неизвестный код —
//!    отказ с названным кодом, а не пропуск строки: пропущенная строка
//!    укорачивает график молча.
//! 3. **Структурная проверка.** Полнота — три независимых утверждения,
//!    и «источник вычитан до конца» полнотой не является.
//!
//! Нарушение инварианта запись снимка **не отменяет**: снимок — то, что
//! источник действительно прислал, и стереть его значит потерять
//! свидетельство. Отменяется пригодность графика к расчёту.

use iaam_core::ids::InstrumentId;
use iaam_market::moex::bondization::parse_bondization_page;
use iaam_market::moex::{PAGE_LIMIT, ScheduleQuery, schedule_request};
use iaam_market::observation::ObservedAt;
use iaam_market::schedule::completeness::{Completeness, validate_moex_profile};
use iaam_market::schedule::{CouponAmount, CouponPeriod, OfferWindow, PrincipalRepayment};
use iaam_store::market::MarketStore;
use iaam_store::schedule::{
    CouponPeriodRow, OfferWindowRow, PrincipalRepaymentRow, ScheduleSnapshotRow,
};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::AppError;
use crate::ports::OutboundHttp;

/// Идентификатор источника графика.
pub const SOURCE_ID: &str = "moex-iss";

/// Потолок числа страниц.
///
/// Предохранитель, а не ожидание: у выпуска с ежемесячным купоном на
/// тридцать лет страниц четыре. Выход по счётчику — отказ с причиной,
/// а не тихий возврат: тихий возврат был бы тем же усечением, только
/// нашими руками.
const MAX_PAGES: u32 = 100;

/// Что синхронизируем.
#[derive(Debug, Clone)]
pub struct ScheduleSyncRequest {
    pub instrument: InstrumentId,
    pub secid: String,
}

/// Наблюдаемое состояние запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSyncResult {
    pub snapshot_id: String,
    /// Записан ли новый снимок. `false` означает, что содержимое совпало
    /// с прошлым, и это не ошибка, а событие следа запуска.
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

/// Синхронизировать график выплат одного выпуска.
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
                "успешный ответ источника",
                &response.status.to_string(),
            ));
        }
        pages_seen.push(start);
        let page = parse_bondization_page(&response.body, observed_at)
            .map_err(|error| invalid("body", "разбираемый график", &error.to_string()))?;
        // Конец выборки — пустая страница ПО ВСЕМ блокам сразу.
        // Смещение общее, и амортизации кончаются раньше купонов.
        if page.total_rows == 0 {
            break;
        }
        coupon_periods.extend(page.coupon_periods);
        principal_repayments.extend(page.principal_repayments);
        offer_windows.extend(page.offer_windows);

        if page_index + 1 == MAX_PAGES {
            return Err(invalid(
                "pages",
                "график короче потолка страниц",
                &MAX_PAGES.to_string(),
            ));
        }
    }

    let repayment_kinds = store
        .market_source_codes(SOURCE_ID, "principal_repayment_kind")
        .map_err(|error| invalid("dictionary", "словарь видов возврата", &error.to_string()))?;
    let offer_kinds = store
        .market_source_codes(SOURCE_ID, "offer_kind")
        .map_err(|error| invalid("dictionary", "словарь видов оферты", &error.to_string()))?;
    let currencies = store
        .market_source_codes(SOURCE_ID, "currency")
        .map_err(|error| invalid("dictionary", "словарь валют", &error.to_string()))?;

    // Неизвестный код — отказ, названный поимённо. Пропуск строки
    // укоротил бы график молча, а «Other» означал бы принятое решение
    // не разбирать — такого решения не принимали.
    for repayment in &principal_repayments {
        if !repayment_kinds.contains_key(&repayment.source_kind) {
            return Err(invalid(
                "principal_repayment_kind",
                "код, известный словарю источника",
                &repayment.source_kind,
            ));
        }
    }
    for window in &offer_windows {
        if !offer_kinds.contains_key(&window.source_kind) {
            return Err(invalid(
                "offer_kind",
                "код, известный словарю источника",
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
                "код валюты, известный словарю источника",
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
        .map_err(|error| invalid("snapshot", "записываемый снимок", &error.to_string()))?;

    let (validated, reason) = match &completeness {
        Completeness::Validated => (true, None),
        Completeness::Incomplete { reason } => (false, Some(reason.clone())),
        // Выпуск вне области применимости профиля: инварианты не
        // применимы, и объявлять их выполненными нельзя.
        Completeness::Unknown => (false, Some("выпуск вне профиля источника".to_owned())),
    };
    store
        .record_schedule_completeness(
            &outcome.snapshot_id,
            true,
            validated,
            reason.as_deref(),
            &pages_seen,
        )
        .map_err(|error| invalid("completeness", "записываемая полнота", &error.to_string()))?;

    Ok(ScheduleSyncResult {
        snapshot_id: outcome.snapshot_id,
        written: outcome.written,
        pages_seen,
        completeness,
    })
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

/// Хэш содержимого снимка.
///
/// Считается по строкам таблиц, а не по телу ответа: тело меняется от
/// полей, которые в домен не входят (текущий номинал в каждой строке,
/// рублёвый эквивалент, число дней до погашения), и хэш по нему объявлял
/// бы изменившимся неизменившийся график каждый день.
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
