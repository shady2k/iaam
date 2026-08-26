//! Отчёты.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::perimeter::{PerimeterPolicy, assess};
use iaam_core::projection::{Projection, ProjectionContext, ProjectionError, advance, project};
use iaam_core::reconciliation::ReconciliationLedger;
use iaam_core::returns::{ReturnsReport, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::FxTable;
use time::{Date, OffsetDateTime};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::Principal;

/// Запрос отчёта о доходности.
#[derive(Debug, Clone)]
pub struct ReturnsQuery {
    pub contour: ContourId,
    pub contour_version: Option<ContourVersion>,
    pub as_of: Option<Date>,
    pub report_currency: CurrencyCode,
    pub fx: FxTable,
    pub lot_rule: LotRuleVersion,
}

/// Отчёт по контуру.
pub async fn returns(
    services: &AppServices,
    principal: &Principal,
    query: &ReturnsQuery,
) -> Result<ReturnsReport, AppError> {
    let version = match query.contour_version {
        Some(version) => version,
        None => services
            .store
            .latest_contour_version(principal.owner, query.contour)
            .await?
            .ok_or_else(|| AppError::NotFound {
                what: "contour",
                id: query.contour.0.to_string(),
            })?,
    };
    // Контур загружается ВМЕСТЕ с владельцем: чужой контур не находится,
    // а не находится и отклоняется потом (§14).
    let definition = services
        .store
        .load_contour(principal.owner, query.contour, version)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "contour_version",
            id: format!("{}/{}", query.contour.0, version.0),
        })?;

    let today = services.clock.today();
    let as_of = query.as_of.unwrap_or(today);
    let projection_events = services
        .store
        .load_events_through(principal.owner, as_of)
        .await?;

    let rules = RuleRegistry::with_defaults();
    let context = ProjectionContext {
        contour: &definition,
        rules: &rules,
        lot_rule: query.lot_rule,
    };

    let projection = build_projection(
        services,
        principal.owner,
        query,
        &definition,
        &projection_events,
        &context,
    )
    .await?;

    if snapshot_may_be_saved(as_of, today) {
        services
            .store
            .save_snapshot(principal.owner, projection.snapshot().clone())
            .await?;
    }

    // Projection is a historical snapshot; reconciliation may use facts
    // received later because they can confirm an earlier period.
    let reconciliation_events = services
        .store
        .load_events_through(principal.owner, Date::MAX)
        .await?;

    report_from_projection(
        &projection,
        query,
        &definition,
        as_of,
        &reconciliation_events,
    )
}

/// Можно ли сохранить снимок, построенный по этому срезу.
///
/// Только для отчёта на сегодня: ключ снимка — контур, его версия и
/// версия правила, поэтому снимок по срезу на прошлую дату лёг бы под
/// тем же ключом и молча подменил бы состояние следующему запросу.
///
/// Вынесено отдельной функцией ради проверяемости: сравнение дат внутри
/// сценария проверяется только через поднятый сервер и базу, а ошибка
/// здесь не выглядит ошибкой — она даёт цифру, просто не ту.
const fn snapshot_may_be_saved(as_of: Date, today: Date) -> bool {
    // `Date` не реализует `PartialEq` в const-контексте через `==`
    // для ссылок, но для значения — реализует.
    as_of.ordinal() == today.ordinal() && as_of.year() == today.year()
}

fn report_from_projection(
    projection: &Projection,
    query: &ReturnsQuery,
    definition: &ContourDefinition,
    as_of: Date,
    reconciliation_events: &[iaam_core::event::Event],
) -> Result<ReturnsReport, AppError> {
    let perimeter = assess(reconciliation_events, PerimeterPolicy::default())?;
    let ledger = ReconciliationLedger::build_with(reconciliation_events, &perimeter.exceptions())?;

    Ok(returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: definition,
            coordinate: iaam_core::returns::KnowledgeCoordinate {
                knowledge_as_of: OffsetDateTime::now_utc(),
                source_priority_version: 1,
                valuation_policy_version: 1,
            },
            as_of,
            report_currency: query.report_currency,
            fx: &query.fx,
            solver_policy: SolverPolicy::returns_default(),
            ledger: &ledger,
            perimeter: &perimeter,
        },
    ))
}

/// Стоит ли пересчитывать журнал целиком после отказа `advance`.
///
/// Снимок — кэш, и его непригодность не является ошибкой работы: почти
/// любой отказ — законный повод пересчитать. Кроме одного: нарушение
/// инварианта пересчёт не исправит, он даст ровно то же самое, и вместо
/// двойной работы отказ уходит наверх с идентификатором корреляции
/// (§15.2).
fn recompute_is_worth_it(error: &ProjectionError) -> bool {
    !error.is_invariant_violation()
}

/// Построение проекции: продвижение снимка, если оно применимо,
/// иначе полный пересчёт.
///
/// Срез передаётся в `advance` **целиком**: решение о том, что уже
/// свёрнуто, принимает ядро. Оболочка не имеет права отбирать «только
/// новое» — событие, пришедшее задним числом до границы снимка, при
/// таком отборе исчезло бы из расчёта молча.
///
/// Любой отказ `advance` — законный повод пересчитать журнал целиком:
/// снимок является кэшем, и его непригодность не является ошибкой
/// работы. Нарушение инварианта при этом никуда не денется — оно
/// проявится и при полном пересчёте.
async fn build_projection(
    services: &AppServices,
    owner: iaam_core::ids::OwnerId,
    query: &ReturnsQuery,
    definition: &ContourDefinition,
    events: &[iaam_core::event::Event],
    context: &ProjectionContext<'_>,
) -> Result<Projection, AppError> {
    let snapshot = services
        .store
        .load_snapshot(owner, definition.id(), definition.version(), query.lot_rule)
        .await?;

    if let Some(snapshot) = snapshot {
        match advance(&snapshot, events, context) {
            Ok(projection) => return Ok(projection),
            Err(error) if !recompute_is_worth_it(&error) => {
                // Нарушение инварианта — не повод пересчитывать: полный
                // пересчёт даст то же самое. Отдаём его наверх, чтобы
                // оно попало в лог с идентификатором корреляции (§15.2).
                return Err(AppError::from_projection(error));
            }
            Err(error) => tracing::info!(
                contour = %definition.id().0,
                reason = error.code(),
                "снимок непригоден, пересчитываем журнал целиком"
            ),
        }
    }

    project(events, context).map_err(AppError::from_projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::projection::invariants::InvariantViolation;
    use time::macros::date;

    #[test]
    fn a_later_opening_assertion_confirms_the_earlier_report() {
        use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
        use iaam_core::event::kind::EventKind;
        use iaam_core::event::leg::Leg;
        use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
        use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
        use iaam_core::ids::{EventId, SourceId};
        use iaam_core::money::{CurrencyCode, Money, PostedMinor};
        use iaam_core::reconciliation::claim::{AssertionPeriod, BalancePoint, ControlClaim};
        use iaam_core::returns::DataQualityStatus;
        use iaam_core::valuation::{FxSource, FxTable};

        let owner = iaam_core::ids::OwnerId::new_random();
        let account = iaam_core::ids::AccountId::new_random();
        let contour = ContourDefinition::new(ContourId::new_random(), ContourVersion(1), [account]);
        let march = AssertionPeriod::between(date!(2026 - 03 - 01), date!(2026 - 03 - 31))
            .unwrap_or_else(|| panic!("valid March period"));
        let april = AssertionPeriod::between(date!(2026 - 04 - 01), date!(2026 - 04 - 30))
            .unwrap_or_else(|| panic!("valid April period"));
        let provenance = Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"a".repeat(64)).unwrap_or_else(|| panic!("valid raw hash")),
            ParserVersion("tinkoff-xlsx/1".to_owned()),
        );
        let later_provenance = Provenance::new(
            SourceId::new_random(),
            RawHash::parse(&"b".repeat(64)).unwrap_or_else(|| panic!("valid later raw hash")),
            ParserVersion("tinkoff-xlsx/1".to_owned()),
        );
        let event = |day, sequence, kind| Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner,
            account,
            kind,
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: Vec::new(),
            provenance: provenance.clone(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        };
        let deposit = Event {
            legs: vec![Leg::cash(
                account,
                Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
            )],
            kind: EventKind::CashIn {
                amount: Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
            },
            ..event(
                date!(2026 - 03 - 02),
                1,
                EventKind::CashIn {
                    amount: Money::new(PostedMinor::new(100_000), CurrencyCode::Rub),
                },
            )
        };
        let march_closing = event(
            date!(2026 - 03 - 31),
            2,
            EventKind::ControlAssertion {
                period: march,
                claim: ControlClaim::CashBalance {
                    currency: CurrencyCode::Rub,
                    amount: PostedMinor::new(100_000),
                    at: BalancePoint::Closing,
                },
            },
        );
        let april_opening = Event {
            provenance: later_provenance,
            ..event(
                date!(2026 - 04 - 30),
                1,
                EventKind::ControlAssertion {
                    period: april,
                    claim: ControlClaim::CashBalance {
                        currency: CurrencyCode::Rub,
                        amount: PostedMinor::new(100_000),
                        at: BalancePoint::Opening,
                    },
                },
            )
        };
        let all_events = vec![deposit, march_closing, april_opening];
        let projection_events: Vec<_> = all_events
            .iter()
            .filter(|event| {
                event
                    .dates
                    .effective_date()
                    .is_some_and(|date| date <= date!(2026 - 03 - 31))
            })
            .cloned()
            .collect();
        let rules = RuleRegistry::with_defaults();
        let context = ProjectionContext {
            contour: &contour,
            rules: &rules,
            lot_rule: LotRuleVersion(1),
        };
        let projection = project(&projection_events, &context)
            .unwrap_or_else(|error| panic!("projection: {error}"));
        let query = ReturnsQuery {
            contour: contour.id(),
            contour_version: Some(contour.version()),
            as_of: Some(date!(2026 - 03 - 31)),
            report_currency: CurrencyCode::Rub,
            fx: FxTable::new(FxSource::OwnerSupplied),
            lot_rule: LotRuleVersion(1),
        };
        let report = report_from_projection(
            &projection,
            &query,
            &contour,
            date!(2026 - 03 - 31),
            &all_events,
        )
        .unwrap_or_else(|error| panic!("report: {error}"));

        assert_eq!(
            report.data_quality.nav_coverage.accepted_internal,
            iaam_core::numeric::decimal::Dec::one()
        );
        assert_eq!(
            report.data_quality.nav_coverage.provisional,
            iaam_core::numeric::decimal::Dec::zero()
        );
        assert_eq!(report.data_quality.status, DataQualityStatus::Clean);
    }

    #[test]
    fn a_snapshot_is_saved_only_for_a_report_dated_today() {
        // Вчерашний срез под сегодняшним ключом — это подмена состояния
        // следующему запросу, а не экономия.
        let today = date!(2026 - 01 - 01);
        assert!(snapshot_may_be_saved(today, today));
        assert!(!snapshot_may_be_saved(date!(2025 - 12 - 31), today));
        assert!(!snapshot_may_be_saved(date!(2026 - 01 - 02), today));
        // Тот же день другого года — не тот же день.
        assert!(!snapshot_may_be_saved(date!(2025 - 01 - 01), today));
    }

    #[test]
    fn every_failure_except_a_broken_invariant_is_worth_a_full_recompute() {
        // Непригодный снимок — обычное дело: пересчитываем. Нарушенный
        // инвариант пересчёт повторит слово в слово, поэтому отдаём его
        // наверх сразу.
        assert!(recompute_is_worth_it(
            &ProjectionError::SnapshotFingerprintMismatch
        ));
        assert!(recompute_is_worth_it(
            &ProjectionError::SnapshotRuleMismatch {
                snapshot: LotRuleVersion(1),
                requested: LotRuleVersion(2),
            }
        ));
        assert!(!recompute_is_worth_it(&ProjectionError::Invariant(
            InvariantViolation::ZeroExternalFlow {
                event: iaam_core::ids::EventId::new_random(),
            }
        )));
    }
}
