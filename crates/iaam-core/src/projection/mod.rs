//! Проекции журнала со снимками (§3.1).
//!
//! «Весь журнал в память» — умолчание, а не архитектурный инвариант.
//! Поэтому публичный интерфейс с самого начала знает про снимок:
//! [`project`] строит его с нуля, [`advance`] продвигает существующий,
//! и полный пересчёт остаётся эталоном для инкрементального.
//!
//! Снимки и кэш хранит **оболочка**: ядро остаётся без состояния.

pub mod balances;
pub mod flows;
pub mod invariants;
pub mod lots;
pub mod offers;
pub mod state;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::contour::{ContourDefinition, ContourId, ContourVersion};
use crate::dates::EffectiveOrder;
use crate::event::Event;
use crate::event::correction::{CorrectionError, resolve};
use crate::event::kind::EventKind;
use crate::rules::{LotRuleVersion, RuleRegistry};
use crate::valuation::InstrumentPrice;
use balances::BalanceError;
use flows::FlowError;
use invariants::{InvariantReport, InvariantViolation};
use lots::{LotBook, LotError};
use state::{LedgerState, StateHash};

/// Версия формата проекции. Снимок, построенный другой версией,
/// продвигать нельзя: смысл полей мог измениться.
pub const PROJECTION_VERSION: u32 = 2;

/// Неизменяемый вход проекции: границы контура и версии правил.
///
/// `Debug` не выводится: `RuleRegistry` хранит трейт-объекты стратегий,
/// у которых отладочного представления нет и быть не может.
#[derive(Clone, Copy)]
pub struct ProjectionContext<'a> {
    pub contour: &'a ContourDefinition,
    pub rules: &'a RuleRegistry,
    pub lot_rule: LotRuleVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionError {
    #[error("снимок построен версией проекции {found}, текущая — {expected}")]
    SnapshotVersionMismatch { expected: u32, found: u32 },
    #[error("отпечаток снимка не совпадает с его состоянием: снимок собран мимо ядра")]
    SnapshotFingerprintMismatch,
    #[error(
        "снимок построен для контура {snapshot_contour:?} версии {snapshot_version:?}, \
         запрошен {requested_contour:?} версии {requested_version:?}"
    )]
    SnapshotContourMismatch {
        snapshot_contour: ContourId,
        snapshot_version: ContourVersion,
        requested_contour: ContourId,
        requested_version: ContourVersion,
    },
    #[error("снимок построен правилом списания {snapshot:?}, запрошено {requested:?}")]
    SnapshotRuleMismatch {
        snapshot: LotRuleVersion,
        requested: LotRuleVersion,
    },
    #[error(
        "действующий журнал до границы снимка изменился: снимок продвигать нельзя, \
         нужен полный пересчёт"
    )]
    PrefixChanged {
        expected: StateHash,
        found: StateHash,
    },
    #[error(transparent)]
    Correction(#[from] CorrectionError),
    #[error(transparent)]
    Balance(#[from] BalanceError),
    #[error(transparent)]
    Lot(#[from] LotError),
    #[error(transparent)]
    Flow(#[from] FlowError),
    #[error(transparent)]
    Invariant(#[from] InvariantViolation),
}

impl ProjectionError {
    /// Машиночитаемый код для API и логов.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SnapshotVersionMismatch { .. } => "snapshot_version_mismatch",
            Self::SnapshotFingerprintMismatch => "snapshot_fingerprint_mismatch",
            Self::SnapshotContourMismatch { .. } => "snapshot_contour_mismatch",
            Self::SnapshotRuleMismatch { .. } => "snapshot_rule_mismatch",
            Self::PrefixChanged { .. } => "prefix_changed",
            Self::Correction(_) => "correction",
            Self::Balance(_) => "balance",
            Self::Lot(_) => "lot",
            Self::Flow(_) => "flow",
            Self::Invariant(_) => "invariant",
        }
    }

    /// Отличает нарушение инварианта от неполноты входа (§15.2).
    #[must_use]
    pub const fn is_invariant_violation(&self) -> bool {
        matches!(self, Self::Invariant(_))
    }
}

/// Снимок состояния на границе `through`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    projection_version: u32,
    contour: ContourId,
    contour_version: ContourVersion,
    lot_rule: LotRuleVersion,
    through: Option<EffectiveOrder>,
    state: LedgerState,
    fingerprint: StateHash,
    /// Отпечаток действующего журнала, свёрнутого в этот снимок.
    prefix_digest: StateHash,
}

impl Snapshot {
    #[must_use]
    pub const fn projection_version(&self) -> u32 {
        self.projection_version
    }

    #[must_use]
    pub const fn contour(&self) -> ContourId {
        self.contour
    }

    #[must_use]
    pub const fn contour_version(&self) -> ContourVersion {
        self.contour_version
    }

    #[must_use]
    pub const fn lot_rule(&self) -> LotRuleVersion {
        self.lot_rule
    }

    #[must_use]
    pub const fn through(&self) -> Option<EffectiveOrder> {
        self.through
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        &self.state
    }

    #[must_use]
    pub const fn fingerprint(&self) -> StateHash {
        self.fingerprint
    }

    /// Отпечаток свёрнутого префикса журнала. Позволяет отличить
    /// «журнал тот же» от «журнал изменился до границы снимка».
    #[must_use]
    pub const fn prefix_digest(&self) -> StateHash {
        self.prefix_digest
    }
}

/// Разобранный снимок.
///
/// Существует ради хранилища: снимок кладётся в базу по частям и
/// собирается обратно. Собранный таким образом снимок ядро проверяет
/// отпечатком — оболочка могла собрать его неверно или не полностью.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotParts {
    pub projection_version: u32,
    pub contour: ContourId,
    pub contour_version: ContourVersion,
    pub lot_rule: LotRuleVersion,
    pub through: Option<EffectiveOrder>,
    pub state: LedgerState,
    pub fingerprint: StateHash,
    pub prefix_digest: StateHash,
}

impl Snapshot {
    /// Сборка снимка из сохранённых частей. Отпечаток **не** пересчитывается:
    /// смысл проверки в `advance` именно в том, чтобы сравнить заявленный
    /// отпечаток с фактическим состоянием.
    #[must_use]
    pub fn restore(parts: SnapshotParts) -> Self {
        Self {
            projection_version: parts.projection_version,
            contour: parts.contour,
            contour_version: parts.contour_version,
            lot_rule: parts.lot_rule,
            through: parts.through,
            state: parts.state,
            fingerprint: parts.fingerprint,
            prefix_digest: parts.prefix_digest,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> SnapshotParts {
        SnapshotParts {
            projection_version: self.projection_version,
            contour: self.contour,
            contour_version: self.contour_version,
            lot_rule: self.lot_rule,
            through: self.through,
            state: self.state,
            fingerprint: self.fingerprint,
            prefix_digest: self.prefix_digest,
        }
    }
}

/// Результат проекции: снимок плюс перечень проверенных инвариантов.
#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    snapshot: Snapshot,
    invariants: InvariantReport,
}

impl Projection {
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    #[must_use]
    pub const fn state(&self) -> &LedgerState {
        self.snapshot.state()
    }

    #[must_use]
    pub const fn invariants(&self) -> &InvariantReport {
        &self.invariants
    }

    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }
}

/// Полный пересчёт с нуля. Эталон для [`advance`].
pub fn project(events: &[Event], ctx: &ProjectionContext) -> Result<Projection, ProjectionError> {
    let state = LedgerState::new(LotBook::new(ctx.lot_rule));
    let effective = resolve(events)?;
    fold(state, &[], &effective, ctx)
}

/// Продвижение снимка **полным срезом журнала**.
///
/// Принимает тот же срез, что и [`project`], а не «пачку новых событий».
/// Это не удобство, а требование корректности: событие, добавленное
/// задним числом до границы снимка, не меняет ни границу, ни состояние
/// снимка. Вызывающий, отбирающий «всё, что позже границы», молча
/// потеряет такое событие — и получит правдоподобные, но неверные
/// остатки, лоты и доходность. Проверено ревью: ровно этот дефект был
/// в первой редакции этого модуля.
///
/// Поэтому решение о применимости снимка принимает ядро: оно сворачивает
/// действующий набор, сравнивает отпечаток префикса и продвигает состояние
/// только тем, что за границей. Несовпадение префикса — не ошибка работы,
/// а сигнал «нужен полный пересчёт»; сторнирование события внутри снимка
/// проявляется именно так, потому что удаляет его из действующего набора.
pub fn advance(
    previous: &Snapshot,
    events: &[Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    if previous.projection_version != PROJECTION_VERSION {
        return Err(ProjectionError::SnapshotVersionMismatch {
            expected: PROJECTION_VERSION,
            found: previous.projection_version,
        });
    }
    if previous.contour != ctx.contour.id() || previous.contour_version != ctx.contour.version() {
        return Err(ProjectionError::SnapshotContourMismatch {
            snapshot_contour: previous.contour,
            snapshot_version: previous.contour_version,
            requested_contour: ctx.contour.id(),
            requested_version: ctx.contour.version(),
        });
    }
    if previous.lot_rule != ctx.lot_rule {
        return Err(ProjectionError::SnapshotRuleMismatch {
            snapshot: previous.lot_rule,
            requested: ctx.lot_rule,
        });
    }
    if previous.state.fingerprint() != previous.fingerprint {
        return Err(ProjectionError::SnapshotFingerprintMismatch);
    }

    let effective = resolve(events)?;
    let split = match previous.through {
        None => 0,
        Some(through) => effective.partition_point(|event| event.order <= through),
    };
    let (prefix, suffix) = effective.split_at(split);

    let found = state::prefix_digest(prefix);
    if found != previous.prefix_digest {
        return Err(ProjectionError::PrefixChanged {
            expected: previous.prefix_digest,
            found,
        });
    }

    fold(previous.state.clone(), prefix, suffix, ctx)
}

/// Применение действующего набора событий к состоянию.
///
/// Три независимых читателя журнала — остатки, лоты, потоки — вызываются
/// по очереди для каждого события. Общих вспомогательных функций у них
/// нет намеренно: инвариант «сумма лотов равна позиции» держится ровно
/// на этой независимости (§15.4).
fn fold(
    mut state: LedgerState,
    already_applied: &[&Event],
    effective: &[&Event],
    ctx: &ProjectionContext,
) -> Result<Projection, ProjectionError> {
    let mut through = already_applied.last().map(|event| event.order);
    for event in effective {
        {
            let (balances, book, flows) = state.parts_mut();
            balances.apply(event)?;
            book.apply(event, ctx.rules)?;
            flows.apply(event, ctx.contour)?;
        }
        if let EventKind::Valuation {
            instrument,
            price,
            currency,
            quality,
        } = &event.kind
        {
            if let Some(as_of) = event.dates.effective_date() {
                state.prices_mut().record(InstrumentPrice {
                    instrument: *instrument,
                    price: *price,
                    currency: *currency,
                    quality: *quality,
                    as_of,
                });
            }
        }
        state.observe(event);
        through = Some(event.order);
    }

    // Инварианты проверяются по всему действующему набору, а не только
    // по продвинутой части: состояние общее, и нарушение могло прийти
    // из снимка, которому ядро не обязано верить (§15.2).
    let all: Vec<&Event> = already_applied
        .iter()
        .chain(effective.iter())
        .copied()
        .collect();
    let invariants = invariants::check(&state, &all)?;
    let fingerprint = state.fingerprint();
    let prefix_digest = state::prefix_digest(&all);
    Ok(Projection {
        snapshot: Snapshot {
            projection_version: PROJECTION_VERSION,
            contour: ctx.contour.id(),
            contour_version: ctx.contour.version(),
            lot_rule: ctx.lot_rule,
            through,
            state,
            fingerprint,
            prefix_digest,
        },
        invariants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contour::{ContourId, ContourVersion};
    use crate::event::Relation;
    use crate::event::kind::EventKind;
    use crate::event::leg::Leg;
    use crate::event::test_support::event_with;
    use crate::ids::{AccountId, CustodyId, InstrumentId};
    use crate::money::{CurrencyCode, Money, PostedMinor, Quantity};
    use crate::numeric::decimal::Dec;
    use crate::rules::RuleRegistry;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn qty(units: i64) -> Quantity {
        Quantity(Dec::new(Decimal::from(units)))
    }

    fn contour_of(account: AccountId) -> ContourDefinition {
        ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account])
    }

    fn deposits(account: AccountId) -> Vec<Event> {
        (1..=4)
            .map(|i| {
                let amount = rub(i64::from(i) * 10_000);
                event_with(
                    account,
                    date!(2025 - 01 - 01) + time::Duration::days(i64::from(i)),
                    i,
                    EventKind::CashIn { amount },
                    vec![Leg::cash(account, amount)],
                )
            })
            .collect()
    }

    #[test]
    fn a_snapshot_reports_its_version_and_its_boundary() {
        // Версия проекции и граница снимка — контракт хранилища: по ним
        // оболочка решает, годится ли снимок вообще. Молчаливый ноль
        // вместо версии сделал бы негодный снимок пригодным.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };

        let empty = project(&[], &ctx).unwrap();
        assert_eq!(empty.snapshot.projection_version(), PROJECTION_VERSION);
        assert_eq!(
            empty.snapshot.through(),
            None,
            "у пустого журнала границы нет"
        );

        let events = deposits(account);
        let full = project(&events, &ctx).unwrap();
        assert_eq!(full.snapshot.projection_version(), PROJECTION_VERSION);
        assert_eq!(
            full.snapshot.through(),
            Some(events[3].order),
            "граница — порядок последнего действующего события"
        );

        // Снимок, пришедший из хранилища, несёт СВОЮ версию, а не версию
        // текущего кода: именно на этом различии держится отказ
        // `SnapshotVersionMismatch`. Аксессор, возвращающий константу,
        // сделал бы снимок чужой версии пригодным.
        let foreign = Snapshot::restore(SnapshotParts {
            projection_version: PROJECTION_VERSION + 41,
            ..full.snapshot.into_parts()
        });
        assert_eq!(foreign.projection_version(), PROJECTION_VERSION + 41);
        assert!(matches!(
            advance(&foreign, &events, &ctx),
            Err(ProjectionError::SnapshotVersionMismatch { .. })
        ));
    }

    #[test]
    fn only_an_invariant_violation_is_reported_as_one() {
        // §15.2 требует отличать нарушение инварианта от неполноты входа:
        // первое отменяет отчёт, второе помечает его невычислимым.
        let mismatched = ProjectionError::SnapshotRuleMismatch {
            snapshot: LotRuleVersion(1),
            requested: LotRuleVersion(2),
        };
        assert!(!mismatched.is_invariant_violation());
        assert!(!ProjectionError::SnapshotFingerprintMismatch.is_invariant_violation());
        assert!(
            ProjectionError::Invariant(InvariantViolation::LotsDoNotMatchPosition {
                key: crate::projection::lots::LotKey {
                    account: AccountId::new_random(),
                    instrument: InstrumentId::new_random(),
                },
                lots: "1".into(),
                position: "2".into(),
            })
            .is_invariant_violation()
        );
    }

    #[test]
    fn advancing_a_snapshot_equals_a_full_recompute() {
        // Инкрементальный путь обязан совпадать с эталонным: снимок —
        // оптимизация, а не другая модель (§3.1).
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);

        let full = project(&events, &ctx).unwrap();
        let head = project(&events[..2], &ctx).unwrap();
        // Срез передаётся целиком: ядро само решает, что уже свёрнуто.
        let advanced = advance(head.snapshot(), &events, &ctx).unwrap();

        assert_eq!(
            full.snapshot().fingerprint(),
            advanced.snapshot().fingerprint()
        );
        assert_eq!(full.snapshot().through(), advanced.snapshot().through());
        assert_eq!(
            full.snapshot().prefix_digest(),
            advanced.snapshot().prefix_digest()
        );
    }

    #[test]
    fn import_order_does_not_change_the_projection() {
        // Свойство §15.3: проекция зависит от EffectiveOrder, а не от того,
        // в каком порядке загрузили файлы.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let mut shuffled = events.clone();
        shuffled.reverse();

        assert_eq!(
            project(&events, &ctx).unwrap().snapshot().fingerprint(),
            project(&shuffled, &ctx).unwrap().snapshot().fingerprint()
        );
    }

    #[test]
    fn a_tampered_snapshot_is_rejected() {
        // Снимок хранит оболочка; ядро не обязано ей верить.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // Оболочка собрала снимок из частей и подставила чужое состояние,
        // оставив прежний отпечаток.
        let other = project(&events[..2], &ctx).unwrap().into_snapshot();
        let mut parts = snapshot.into_parts();
        parts.state = other.into_parts().state;
        let tampered = Snapshot::restore(parts);

        assert!(matches!(
            advance(&tampered, &events, &ctx),
            Err(ProjectionError::SnapshotFingerprintMismatch)
        ));
    }

    #[test]
    fn an_event_inserted_before_the_snapshot_boundary_forces_a_full_recompute() {
        // Самый опасный случай: событие пришло задним числом и встало
        // ДО границы снимка. Оно не меняет ни границу, ни состояние
        // снимка, поэтому наивное «взять всё, что позже границы» молча
        // потеряло бы его — и выдало бы правдоподобные, но неверные
        // остатки. Ядро обязано это заметить.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events, &ctx).unwrap().into_snapshot();

        // Забытое пополнение с датой в середине уже свёрнутого периода.
        let forgotten = event_with(
            account,
            date!(2025 - 01 - 02),
            99,
            EventKind::CashIn { amount: rub(777) },
            vec![Leg::cash(account, rub(777))],
        );
        let mut with_backdated = events.clone();
        with_backdated.push(forgotten);

        let error = advance(&snapshot, &with_backdated, &ctx).unwrap_err();
        assert!(
            matches!(error, ProjectionError::PrefixChanged { .. }),
            "ожидалось PrefixChanged, получено {error}"
        );

        // Полный пересчёт видит забытое событие.
        let recomputed = project(&with_backdated, &ctx).unwrap();
        assert_eq!(
            recomputed
                .state()
                .balances()
                .cash(account, CurrencyCode::Rub),
            Some(rub(10_000 + 20_000 + 30_000 + 40_000 + 777))
        );
    }

    #[test]
    fn reversing_an_event_inside_the_snapshot_forces_a_full_recompute() {
        // Сторнирование удаляет событие из действующего набора, то есть
        // меняет уже свёрнутый префикс. Вычесть его из агрегата нельзя,
        // и притвориться, что можно, значит тихо потерять исправление.
        let account = AccountId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let events = deposits(account);
        let snapshot = project(&events[..2], &ctx).unwrap().into_snapshot();

        let mut with_reversal = events.clone();
        with_reversal[3].relation = Relation::Reversal {
            target: events[0].id,
        };

        assert!(matches!(
            advance(&snapshot, &with_reversal, &ctx),
            Err(ProjectionError::PrefixChanged { .. })
        ));
    }

    #[test]
    fn a_snapshot_of_another_contour_is_rejected() {
        let account = AccountId::new_random();
        let rules = RuleRegistry::with_defaults();
        let first = contour_of(account);
        let second = contour_of(account);
        let events = deposits(account);
        let snapshot = project(
            &events,
            &ProjectionContext {
                contour: &first,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            },
        )
        .unwrap()
        .into_snapshot();

        assert!(matches!(
            advance(
                &snapshot,
                &events,
                &ProjectionContext {
                    contour: &second,
                    rules: &rules,
                    lot_rule: LotRuleVersion(1),
                }
            ),
            Err(ProjectionError::SnapshotContourMismatch { .. })
        ));
    }

    #[test]
    fn a_leg_contradicting_the_event_never_reaches_the_projection() {
        // Событие, чья нога говорит не то, что тип события, отклоняется
        // входным заслоном — до того, как попадёт в append-only журнал.
        // Инвариант «сумма лотов равна позиции» остаётся вторым рубежом:
        // он ловит то же расхождение, если оно придёт из хранилища,
        // наполненного в обход приёмки (§15.2).
        let account = AccountId::new_random();
        let instrument = InstrumentId::new_random();
        let contour = contour_of(account);
        let rules = RuleRegistry::with_defaults();
        let ctx = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let event = event_with(
            account,
            date!(2025 - 04 - 01),
            1,
            EventKind::Trade {
                side: crate::event::kind::TradeSide::Buy,
                instrument,
                quantity: qty(100),
                gross: rub(1_000_000),
                fee: None,
                accrued_interest: None,
            },
            vec![
                Leg::cash(account, rub(-1_000_000)),
                // Нога говорит 90 бумаг, тип события — 100.
                Leg::security(account, CustodyId::new_random(), instrument, qty(90)),
            ],
        );

        // Заслон формы события отклоняет противоречие сам по себе.
        assert!(matches!(
            event.validate_structure(),
            Err(crate::event::EventValidationError::LegDoesNotMatchEvent { .. })
        ));

        // И проекция такого события не строит: она перепроверяет форму,
        // потому что не обязана верить тому, что лежит в хранилище.
        let error = project(&[event], &ctx).unwrap_err();
        assert!(error.is_invariant_violation(), "{error}");
        assert_eq!(error.code(), "invariant");
    }
}
