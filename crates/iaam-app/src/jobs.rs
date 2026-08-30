//! Market series synchronisation scheduling subsystem.
//!
//! There is no general-purpose job engine here: only one type of work is
//! registered — market synchronisation. The run policy itself is pure and therefore
//! is tested without sleeping, network access or background threads.

use std::sync::{Arc, Mutex};

use iaam_core::event::Event;
use iaam_core::event::kind::EventKind;
use iaam_core::ids::InstrumentId;
use iaam_core::projection::active_instruments;
use time::{Date, Duration, OffsetDateTime, Time};

use crate::AppServices;
use crate::error::AppError;
use crate::sync::{MarketSource, MarketSyncRequest, MarketSyncResult, sync_market};

/// Window during which the source may correct already published results.
pub const CORRECTION_WINDOW_DAYS: i64 = 21;
/// Buffer before the first event: it covers weekends and public holidays.
pub const INITIAL_PADDING_DAYS: i64 = 7;
/// Time after which the market's daily results are considered published.
#[must_use]
pub fn default_close_time() -> Time {
    Time::from_hms(19, 0, 0).expect("valid close time")
}

/// Daily synchronisation settings for one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketSchedule {
    close_time: Time,
    correction_window_days: i64,
    initial_padding_days: i64,
}

impl Default for MarketSchedule {
    fn default() -> Self {
        Self {
            close_time: default_close_time(),
            correction_window_days: CORRECTION_WINDOW_DAYS,
            initial_padding_days: INITIAL_PADDING_DAYS,
        }
    }
}

impl MarketSchedule {
    #[must_use]
    pub const fn new(
        close_time: Time,
        correction_window_days: i64,
        initial_padding_days: i64,
    ) -> Self {
        Self {
            close_time,
            correction_window_days,
            initial_padding_days,
        }
    }

    /// Pure «is it time to run?» decision without sleeping or side effects.
    #[must_use]
    pub fn should_run(self, now: OffsetDateTime, state: &ScheduleState) -> bool {
        state.active && now.time() >= self.close_time && state.last_run != Some(now.date())
    }

    /// Range for an automatic run.
    ///
    /// The first run starts from the first event, and subsequent runs — from
    /// the rolling correction boundary. `None` means a closed position.
    #[must_use]
    pub fn window(
        self,
        today: Date,
        first_event: Date,
        state: &ScheduleState,
    ) -> Option<(Date, Date)> {
        if !state.active {
            return None;
        }
        let from = match state.last_run {
            Some(_) => today - Duration::days(self.correction_window_days.max(0)),
            None => first_event - Duration::days(self.initial_padding_days.max(0)),
        };
        Some((from.min(today), today))
    }

    /// Range for a manual run: a manual invocation does not wait for trading to close.
    #[must_use]
    pub fn manual_window(
        self,
        now: OffsetDateTime,
        first_event: Date,
        state: &ScheduleState,
    ) -> Option<(Date, Date)> {
        self.window(now.date(), first_event, state)
    }
}

/// State of one registered series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleState {
    active: bool,
    last_run: Option<Date>,
}

impl ScheduleState {
    #[must_use]
    pub const fn never() -> Self {
        Self {
            active: true,
            last_run: None,
        }
    }

    #[must_use]
    pub const fn after(date: Date) -> Self {
        Self {
            active: true,
            last_run: Some(date),
        }
    }

    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            active: false,
            last_run: None,
        }
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        self.active
    }

    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    #[must_use]
    pub const fn last_run(self) -> Option<Date> {
        self.last_run
    }
}

/// Instrument's first event date, or the first journal date for general series.
#[must_use]
pub fn first_event_date(events: &[Event], instrument: Option<InstrumentId>) -> Option<Date> {
    events
        .iter()
        .filter(|event| match (&event.kind, instrument) {
            (
                EventKind::Trade {
                    instrument: event_instrument,
                    ..
                }
                | EventKind::OpeningPosition {
                    instrument: event_instrument,
                    ..
                },
                Some(instrument),
            ) => *event_instrument == instrument,
            (_, None) => true,
            _ => false,
        })
        .filter_map(|event| event.dates.effective_date())
        .min()
}

/// One market series and its daily synchronisation.
pub struct MarketSyncJob {
    services: Arc<AppServices>,
    source: MarketSource,
    first_event: Mutex<Date>,
    schedule: MarketSchedule,
    state: Mutex<ScheduleState>,
}

impl MarketSyncJob {
    #[must_use]
    pub fn new(services: Arc<AppServices>, source: MarketSource, first_event: Date) -> Self {
        Self::with_schedule(services, source, first_event, MarketSchedule::default())
    }

    #[must_use]
    pub fn with_schedule(
        services: Arc<AppServices>,
        source: MarketSource,
        first_event: Date,
        schedule: MarketSchedule,
    ) -> Self {
        Self {
            services,
            source,
            first_event: Mutex::new(first_event),
            schedule,
            state: Mutex::new(ScheduleState::never()),
        }
    }

    fn source_instrument(&self) -> Option<InstrumentId> {
        match self.source {
            MarketSource::Moex { instrument, .. } => Some(instrument),
            MarketSource::CbrDaily | MarketSource::CbrDynamic { .. } | MarketSource::CbrKeyRate => {
                None
            }
        }
    }

    fn set_first_event(&self, first_event: Date) {
        *self
            .first_event
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = first_event;
    }

    pub fn set_active(&self, active: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.set_active(active);
    }

    #[must_use]
    pub fn state(&self) -> ScheduleState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run the series if the daily window has already opened.
    pub async fn run_if_due(
        &self,
        now: OffsetDateTime,
    ) -> Result<Option<MarketSyncResult>, AppError> {
        let window = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let first_event = *self
                .first_event
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !self.schedule.should_run(now, &state) {
                return Ok(None);
            }
            self.schedule.window(now.date(), first_event, &state)
        };
        let Some((from, to)) = window else {
            return Ok(None);
        };
        let result = self.run(from, to).await;
        if result.is_ok() {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_run = Some(now.date());
        }
        result.map(Some)
    }

    /// Run the series manually regardless of the time of day.
    pub async fn run_now(&self, now: OffsetDateTime) -> Result<Option<MarketSyncResult>, AppError> {
        let window = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let first_event = *self
                .first_event
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.schedule.manual_window(now, first_event, &state)
        };
        let Some((from, to)) = window else {
            return Ok(None);
        };
        let result = self.run(from, to).await;
        if result.is_ok() {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_run = Some(now.date());
        }
        result.map(Some)
    }

    async fn run(&self, from: Date, to: Date) -> Result<MarketSyncResult, AppError> {
        let request = MarketSyncRequest {
            source: self.source.clone(),
            from,
            to,
        };
        let mut store = self.services.market_store.lock().await;
        sync_market(&mut store, self.services.http.as_ref(), request).await
    }
}

/// Scheduler for market series only. No other job types are added here.
pub struct MarketScheduler {
    services: Arc<AppServices>,
    jobs: Arc<Mutex<Vec<Arc<MarketSyncJob>>>>,
}

impl MarketScheduler {
    #[must_use]
    pub fn new(services: Arc<AppServices>) -> Self {
        Self {
            services,
            jobs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn register(&self, job: Arc<MarketSyncJob>) {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(job);
    }

    fn deactivate_all(&self) {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for job in jobs {
            job.set_active(false);
        }
    }

    async fn refresh_from_journal(&self) -> Result<(), AppError> {
        let owner = match self.services.tokens.sole_owner().await? {
            crate::ports::SoleOwner::Single(owner) => owner,
            crate::ports::SoleOwner::None | crate::ports::SoleOwner::Several => {
                self.deactivate_all();
                return Ok(());
            }
        };
        let events = self
            .services
            .store
            .load_events_through(owner, Date::MAX)
            .await?;
        let active = active_instruments(&events).map_err(AppError::Schedule)?;
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for job in jobs {
            if let Some(instrument) = job.source_instrument() {
                job.set_active(active.contains(&instrument));
                if let Some(first) = first_event_date(&events, Some(instrument)) {
                    job.set_first_event(first);
                }
            } else {
                job.set_active(!events.is_empty());
                if let Some(first) = first_event_date(&events, None) {
                    job.set_first_event(first);
                }
            }
        }
        Ok(())
    }

    pub async fn tick(&self, now: OffsetDateTime) -> Vec<Result<MarketSyncResult, AppError>> {
        let has_jobs = !self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty();
        if !has_jobs {
            return Vec::new();
        }
        if let Err(error) = self.refresh_from_journal().await {
            return vec![Err(error)];
        }
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut results = Vec::new();
        for job in jobs {
            match job.run_if_due(now).await {
                Ok(Some(result)) => results.push(Ok(result)),
                Ok(None) => {}
                Err(error) => results.push(Err(error)),
            }
        }
        results
    }

    /// Start minute-by-minute polling. The decision whether to actually run remains
    /// in the pure `MarketSchedule::should_run`, so tests do not need sleep.
    #[must_use]
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = self.tick(OffsetDateTime::now_utc()).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;
    use time::{OffsetDateTime, Time, UtcOffset};

    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::corporate_action::CorporateAction;
    use iaam_core::event::kind::{EventKind, TradeSide};
    use iaam_core::event::leg::Leg;
    use iaam_core::event::offer::OfferExerciseAction;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{AccountId, CustodyId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use rust_decimal::Decimal;

    fn qty(n: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(n)))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn event_of(kind: EventKind, legs: Vec<Leg>) -> Event {
        let account = AccountId::new_random();
        let day = date!(2026 - 06 - 15);
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, 0),
            legs,
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"c".repeat(64)).unwrap(),
                ParserVersion("test/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn bought(instrument: InstrumentId, quantity: i64) -> Event {
        event_of(
            EventKind::Trade {
                side: TradeSide::Buy,
                instrument,
                quantity: qty(quantity),
                gross: rub(-1_000_000),
                fee: None,
                accrued_interest: None,
            },
            Vec::new(),
        )
    }

    fn amortised(instrument: InstrumentId) -> Event {
        event_of(
            EventKind::CorporateAction {
                action: CorporateAction::PartialRedemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(10),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(200)),
                        CurrencyCode::Rub,
                    ),
                    compensation: rub(2_000),
                    effective_date: date!(2026 - 06 - 15),
                    record_date: None,
                    grounds: None,
                    basis_allocation: iaam_core::event::allocation::BasisAllocation::default(),
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn amortisation_does_not_change_the_position_count() {
        // §6.5: amortisation pays out cash, but the quantity of securities
        // does not decrease. A negative delta would stop synchronising
        // the price of an active security.
        let instrument = InstrumentId::new_random();
        let events = vec![bought(instrument, 10), amortised(instrument)];
        assert!(active_instruments(&events).unwrap().contains(&instrument));
    }

    #[test]
    fn a_redeemed_bond_stops_being_active() {
        let instrument = InstrumentId::new_random();
        let redeemed = event_of(
            EventKind::CorporateAction {
                action: CorporateAction::Redemption {
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(10),
                    principal_returned_per_unit: PerUnitAmount::new(
                        Dec::new(Decimal::from(800)),
                        CurrencyCode::Rub,
                    ),
                    compensation: rub(8_000),
                    effective_date: date!(2026 - 12 - 15),
                    record_date: None,
                    grounds: None,
                },
            },
            Vec::new(),
        );
        let events = vec![bought(instrument, 10), redeemed];
        assert!(!active_instruments(&events).unwrap().contains(&instrument));
    }

    #[test]
    fn a_conversion_moves_the_count_from_predecessor_to_successor() {
        // Substitution changes the quantities of two securities at once: reducing it
        // to one would leave the predecessor permanently active.
        let predecessor = InstrumentId::new_random();
        let successor = InstrumentId::new_random();
        let converted = event_of(
            EventKind::CorporateAction {
                action: CorporateAction::Conversion {
                    predecessor,
                    successor,
                    custody: CustodyId::new_random(),
                    ratio: Dec::one(),
                    quantity_in: qty(10),
                    quantity_out: qty(10),
                    fractional:
                        iaam_core::event::corporate_action::FractionalTreatment::NotApplicable,
                    compensation: None,
                    effective_date: date!(2026 - 09 - 01),
                    record_date: None,
                    grounds: None,
                    basis_transfer:
                        iaam_core::event::corporate_action::BasisTransferRule::CarryOver,
                },
            },
            Vec::new(),
        );
        let active = active_instruments(&[bought(predecessor, 10), converted]).unwrap();
        assert!(!active.contains(&predecessor));
        assert!(active.contains(&successor));
    }

    #[test]
    fn a_settled_offer_removes_the_bought_back_quantity() {
        let instrument = InstrumentId::new_random();
        let settled = event_of(
            EventKind::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: iaam_core::event::offer::OfferSubmissionId::new_random(),
                    instrument,
                    custody: CustodyId::new_random(),
                    quantity: qty(10),
                    gross: rub(1_000_000),
                    fee: None,
                    accrued_interest: None,
                },
            },
            Vec::new(),
        );
        let events = vec![bought(instrument, 10), settled];
        assert!(!active_instruments(&events).unwrap().contains(&instrument));
    }

    #[test]
    fn daily_run_is_due_only_after_close_and_once_per_date() {
        let schedule = MarketSchedule::default();
        let before_close = OffsetDateTime::new_in_offset(
            date!(2026 - 08 - 26),
            Time::from_hms(18, 59, 59).unwrap(),
            UtcOffset::UTC,
        );
        assert!(!schedule.should_run(before_close, &ScheduleState::never()));

        let after_close = OffsetDateTime::new_in_offset(
            date!(2026 - 08 - 26),
            Time::from_hms(19, 0, 0).unwrap(),
            UtcOffset::UTC,
        );
        let state = ScheduleState::after(date!(2026 - 08 - 25));
        assert!(schedule.should_run(after_close, &state));
        assert!(!schedule.should_run(after_close, &ScheduleState::after(date!(2026 - 08 - 26)),));
    }

    #[test]
    fn initial_window_starts_before_first_event_and_repeat_uses_correction_window() {
        let schedule = MarketSchedule::default();
        let first_event = date!(2024 - 01 - 15);
        let today = date!(2026 - 08 - 26);
        assert_eq!(
            schedule.window(today, first_event, &ScheduleState::never()),
            Some((date!(2024 - 01 - 08), today)),
        );
        assert_eq!(
            schedule.window(
                today,
                first_event,
                &ScheduleState::after(date!(2026 - 08 - 25)),
            ),
            Some((date!(2026 - 08 - 05), today)),
        );
    }

    #[test]
    fn inactive_position_has_no_window_and_never_becomes_due() {
        let schedule = MarketSchedule::default();
        let state = ScheduleState::inactive();
        assert_eq!(
            schedule.window(date!(2026 - 08 - 26), date!(2024 - 01 - 15), &state,),
            None,
        );
        assert!(!schedule.should_run(
            OffsetDateTime::new_in_offset(
                date!(2026 - 08 - 26),
                Time::from_hms(19, 0, 0).unwrap(),
                UtcOffset::UTC,
            ),
            &state,
        ));
    }

    #[test]
    fn manual_run_ignores_close_time_but_keeps_the_same_window_policy() {
        let schedule = MarketSchedule::default();
        let now = OffsetDateTime::new_in_offset(
            date!(2026 - 08 - 26),
            Time::from_hms(9, 0, 0).unwrap(),
            UtcOffset::UTC,
        );
        assert_eq!(
            schedule.manual_window(now, date!(2024 - 01 - 15), &ScheduleState::never(),),
            Some((date!(2024 - 01 - 08), date!(2026 - 08 - 26))),
        );

        assert_eq!(
            schedule.manual_window(
                now,
                date!(2024 - 01 - 15),
                &ScheduleState::after(date!(2026 - 08 - 25)),
            ),
            Some((date!(2026 - 08 - 05), date!(2026 - 08 - 26))),
        );
    }
}
