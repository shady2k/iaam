//! Подсистема планирования синхронизации рыночных рядов.
//!
//! Здесь нет универсального движка заданий: зарегистрирован только один
//! вид работы — синхронизация рынка. Сама политика запуска чистая и потому
//! проверяется без сна, сети и фоновых потоков.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use iaam_core::event::Event;
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::{EventKind, TradeSide};
use iaam_core::event::offer::OfferExerciseAction;
use iaam_core::ids::InstrumentId;
use iaam_core::numeric::decimal::Dec;
use time::{Date, Duration, OffsetDateTime, Time};

use crate::AppServices;
use crate::error::AppError;
use crate::sync::{MarketSource, MarketSyncRequest, MarketSyncResult, sync_market};

/// Окно, в котором источник может исправить уже опубликованные итоги.
pub const CORRECTION_WINDOW_DAYS: i64 = 21;
/// Запас перед первым событием: он покрывает выходные и праздники.
pub const INITIAL_PADDING_DAYS: i64 = 7;
/// Время, после которого дневные итоги рынка считаются опубликованными.
#[must_use]
pub fn default_close_time() -> Time {
    Time::from_hms(19, 0, 0).expect("valid close time")
}

/// Настройки ежедневной синхронизации одной серии.
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

    /// Чистое решение «пора ли запускать» без сна и побочных эффектов.
    #[must_use]
    pub fn should_run(self, now: OffsetDateTime, state: &ScheduleState) -> bool {
        state.active && now.time() >= self.close_time && state.last_run != Some(now.date())
    }

    /// Диапазон для автоматического запуска.
    ///
    /// Первый запуск начинается от первого события, а последующие — от
    /// скользящей границы исправлений. `None` означает закрытую позицию.
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

    /// Диапазон ручного запуска: ручной вызов не ждёт закрытия торгов.
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

/// Состояние одной зарегистрированной серии.
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

/// Смена знака количества. `i64::MIN`-подобного случая у `Dec` нет,
/// но отказ отрицания молча оставил бы количество прежним, поэтому
/// он вынесен в одно место.
fn negated(quantity: Dec) -> Dec {
    quantity.checked_neg().unwrap_or(quantity)
}

/// Инструменты, у которых итоговое количество не равно нулю.
///
/// Это чистая проекция только для выбора расписания. Отчётная проекция
/// остаётся в `iaam-core`; здесь не создаётся второй источник чисел.
#[must_use]
pub fn active_instruments(events: &[Event]) -> BTreeSet<InstrumentId> {
    let mut quantities = BTreeMap::<InstrumentId, Dec>::new();
    for event in events {
        // Список пар, а не одна пара: замещение двигает количество сразу
        // по двум бумагам, и свести его к одной значило бы оставить
        // предшественника вечно активным.
        let deltas: Vec<(InstrumentId, Dec)> = match &event.kind {
            EventKind::Trade {
                side,
                instrument,
                quantity,
                ..
            } => vec![(
                *instrument,
                match side {
                    TradeSide::Buy => quantity.0,
                    TradeSide::Sell => negated(quantity.0),
                },
            )],
            EventKind::OpeningPosition {
                instrument,
                quantity,
                ..
            } => vec![(*instrument, quantity.0)],
            EventKind::CorporateAction { action } => match action {
                // Амортизация выплачивает деньги, но количество бумаг
                // не меняет (§6.5): нулевая дельта, а не пропуск.
                CorporateAction::PartialRedemption { instrument, .. } => {
                    vec![(*instrument, Dec::zero())]
                }
                CorporateAction::Redemption {
                    instrument,
                    quantity,
                    ..
                } => vec![(*instrument, negated(quantity.0))],
                CorporateAction::Conversion {
                    predecessor,
                    successor,
                    quantity_in,
                    quantity_out,
                    ..
                } => vec![
                    (*predecessor, negated(quantity_in.0)),
                    (*successor, quantity_out.0),
                ],
            },
            EventKind::OfferExercise { action } => match action {
                // Подача и отзыв заявки бумаг не двигают.
                OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
                    Vec::new()
                }
                OfferExerciseAction::Settled {
                    instrument,
                    quantity,
                    ..
                } => vec![(*instrument, negated(quantity.0))],
            },
            EventKind::CashIn { .. }
            | EventKind::CashOut { .. }
            | EventKind::CashTransfer { .. }
            | EventKind::Income { .. }
            | EventKind::Fee { .. }
            | EventKind::OpeningCash { .. }
            | EventKind::Valuation { .. }
            | EventKind::ControlAssertion { .. } => Vec::new(),
        };
        for (instrument, delta) in deltas {
            let current = quantities.entry(instrument).or_insert_with(Dec::zero);
            *current = current.checked_add(delta).unwrap_or(*current);
        }
    }
    quantities
        .into_iter()
        .filter_map(|(instrument, quantity)| (!quantity.is_zero()).then_some(instrument))
        .collect()
}

/// Первая дата события инструмента, либо первая дата журнала для общих рядов.
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

/// Одна рыночная серия и её ежедневная синхронизация.
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

    /// Выполнить серию, если ежедневное окно уже открылось.
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

    /// Ручной запуск серии независимо от времени суток.
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
        sync_market(&mut store, self.services.market.as_ref(), request).await
    }
}

/// Планировщик только рыночных серий. Другие виды заданий сюда не добавляются.
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
        let active = active_instruments(&events);
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

    /// Запустить минутный опрос. Решение о фактическом запуске остаётся
    /// чистым `MarketSchedule::should_run`, поэтому тестам не нужен sleep.
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
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{AccountId, CustodyId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
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
                },
            },
            Vec::new(),
        )
    }

    #[test]
    fn amortisation_does_not_change_the_position_count() {
        // §6.5: амортизация выплачивает деньги, но количество бумаг
        // не уменьшает. Отрицательная дельта закрыла бы синхронизацию
        // цены живой бумаги.
        let instrument = InstrumentId::new_random();
        let events = vec![bought(instrument, 10), amortised(instrument)];
        assert!(active_instruments(&events).contains(&instrument));
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
        assert!(!active_instruments(&events).contains(&instrument));
    }

    #[test]
    fn a_conversion_moves_the_count_from_predecessor_to_successor() {
        // Замещение двигает количество по двум бумагам сразу: свести его
        // к одной значило бы оставить предшественника вечно активным.
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
        let active = active_instruments(&[bought(predecessor, 10), converted]);
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
        assert!(!active_instruments(&events).contains(&instrument));
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
