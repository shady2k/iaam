//! Отчёты.

use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::approx::SolverPolicy;
use iaam_core::projection::{Projection, ProjectionContext, ProjectionError, advance, project};
use iaam_core::returns::{ReturnsReport, ReturnsRequest, returns_report};
use iaam_core::rules::{LotRuleVersion, RuleRegistry};
use iaam_core::valuation::FxTable;
use time::Date;

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
    let events = services
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
        &events,
        &context,
    )
    .await?;

    if snapshot_may_be_saved(as_of, today) {
        services
            .store
            .save_snapshot(principal.owner, projection.snapshot().clone())
            .await?;
    }

    Ok(returns_report(
        projection.state(),
        &ReturnsRequest {
            contour: &definition,
            as_of,
            report_currency: query.report_currency,
            fx: &query.fx,
            solver_policy: SolverPolicy::returns_default(),
        },
    ))
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
