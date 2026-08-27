//! Приёмка журнальных фактов: корпоративных действий и оферты
//! (§4.7, §3.5).
//!
//! Отдельный вход, а не новые члены `OperationKind`, и причина
//! механическая, а не вкусовая: [`crate::operation::OperationDates`]
//! жёстко проставляет `entitlement: None`, то есть операционная модель
//! дат не умеет выразить дату фиксации реестра вовсе. У корпоративного
//! действия она — часть факта. Расширение операций потребовало бы либо
//! деформировать общую модель дат, либо носить даты в двух местах сразу.
//!
//! Оферта здесь рядом с корпоративными действиями как **сосед**,
//! а не как член семейства: `event/offer.rs` фиксирует, что оферта —
//! право владельца, а не решение эмитента. Общее у них — канал приёмки
//! и журнал, а не природа факта.
//!
//! Как и в операциях, **знаки и ноги строит приёмка**: клиент присылает
//! положительные величины, а отрицательное количество выбывающей бумаги
//! и денежный итог расчёта считаются здесь.

use iaam_core::dates::{EffectiveOrder, EntitlementDate, EventDates, SettledDate, TradeDate};
use iaam_core::event::corporate_action::CorporateAction;
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::offer::OfferExerciseAction;
use iaam_core::event::provenance::{ParserVersion, Provenance};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId};
use iaam_core::money::{Money, Quantity};
use serde::{Deserialize, Serialize};
use time::Date;

use crate::operation::{NormalizationContext, Normalized};
use crate::verdict::Rejection;

/// Версия разбора журнальных фактов.
///
/// Своя, а не общая с операциями: происхождение обязано называть тот
/// разбор, который событие построил. Одна версия на два непохожих
/// разбора не даст отличить ошибку одного от ошибки другого (§4.1).
pub const PARSER_VERSION: &str = "ingest/journal/1";

/// Журнальный факт, пришедший через API.
///
/// Две семьи под одной крышей — это общий канал приёмки, а не общая
/// природа: корпоративное действие решает эмитент, оферту предъявляет
/// владелец. Приём произвольного `EventKind` здесь невозможен намеренно:
/// вход принимает ровно те семьи, которые перечислены.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JournalFact {
    /// Даты внутри самого факта: у корпоративного действия дата
    /// вступления в силу — часть идентичности, а не свойство подачи.
    CorporateAction(CorporateAction),
    /// У оферты собственной даты нет: `event/offer.rs` описывает
    /// заявку и расчёт, но не день, когда это случилось. Значит, день
    /// присылает клиент — выдумать его приёмке нечем.
    OfferExercise {
        action: OfferExerciseAction,
        day: Date,
    },
}

/// Журнальное событие, поданное на приёмку.
///
/// Полей знака и ног здесь нет: их строит приёмка (см. модульный
/// комментарий), клиент присылает только положительные величины.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmittedJournalEvent {
    pub account: AccountId,
    pub fact: JournalFact,
    /// Ключ идемпотентности клиента (§10.6).
    pub idempotency_key: Option<String>,
    /// Идентификатор факта в источнике, если он есть.
    pub source_operation_id: Option<String>,
}

/// Превращение журнального факта в событие журнала.
///
/// Форму события — какие ноги обязаны быть у каждого члена — проверяет
/// ядро своим `validate_structure()` на общем шве приёмки, а не этот
/// нормализатор. Двойная проверка разошлась бы с ядром молча.
pub fn normalize_journal_event(
    submitted: &SubmittedJournalEvent,
    context: NormalizationContext,
) -> Result<Normalized, Rejection> {
    let (dates, day) = dates_of(&submitted.fact);
    let (kind, legs) = build(submitted.account, &submitted.fact)?;
    // Отпечаток считается там же, где дедупликация: второй экземпляр
    // этой функции разошёлся бы с первым молча (§10.6).
    let raw_hash = crate::dedup::fingerprint_journal_event(submitted);

    Ok(Normalized {
        event: Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: context.owner,
            account: submitted.account,
            kind,
            dates,
            // Временный номер: окончательный ставит хранилище (§4.8).
            order: EffectiveOrder::new(day, 1),
            legs,
            provenance: {
                let base = Provenance::new(
                    context.source,
                    raw_hash,
                    ParserVersion(PARSER_VERSION.to_owned()),
                );
                match submitted.source_operation_id.as_deref() {
                    Some(id) => base.with_source_operation_id(id),
                    None => base,
                }
            },
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: submitted.idempotency_key.clone(),
        },
    })
}

/// Даты события и день, по которому оно попадает в порядок.
///
/// День возвращается рядом с датами, а не достаётся потом через
/// `effective_date()`: у обеих семей он есть по построению, и отказ
/// «дат нет» был бы веткой, в которую нельзя попасть.
fn dates_of(fact: &JournalFact) -> (EventDates, Date) {
    match fact {
        // Дата вступления в силу — это день, когда факт состоялся
        // на счёте: номинал уменьшился, бумага выбыла, замещение
        // произошло. Дата фиксации реестра идёт в `entitlement`:
        // ради неё вход и заведён отдельно от операций.
        JournalFact::CorporateAction(action) => {
            let day = action.effective_date();
            (
                EventDates {
                    settled: Some(SettledDate(day)),
                    entitlement: action.record_date().map(EntitlementDate),
                    ..EventDates::empty()
                },
                day,
            )
        }
        // Подача и отзыв ничего не двигают, поэтому их день — `trade`,
        // день действия владельца, а не расчёта, которого не было.
        JournalFact::OfferExercise {
            action: OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. },
            day,
        } => (
            EventDates {
                trade: Some(TradeDate(*day)),
                ..EventDates::empty()
            },
            *day,
        ),
        JournalFact::OfferExercise {
            action: OfferExerciseAction::Settled { .. },
            day,
        } => (
            EventDates {
                settled: Some(SettledDate(*day)),
                ..EventDates::empty()
            },
            *day,
        ),
    }
}

/// Построение типа события и ног.
///
/// Диспетчер исчерпывающий: новый член семьи обязан сломать сборку.
fn build(account: AccountId, fact: &JournalFact) -> Result<(EventKind, Vec<Leg>), Rejection> {
    let legs = match fact {
        JournalFact::CorporateAction(action) => corporate_action_legs(account, action)?,
        JournalFact::OfferExercise { action, .. } => offer_legs(account, action)?,
    };
    let kind = match fact {
        JournalFact::CorporateAction(action) => EventKind::CorporateAction {
            action: action.clone(),
        },
        JournalFact::OfferExercise { action, .. } => EventKind::OfferExercise {
            action: action.clone(),
        },
    };
    Ok((kind, legs))
}

fn corporate_action_legs(
    account: AccountId,
    action: &CorporateAction,
) -> Result<Vec<Leg>, Rejection> {
    match action {
        // Одна нога `Principal` и ни одной бумажной: количество бумаг
        // амортизация не меняет (§6.5). Пары «Cash + Principal» нет —
        // `Principal` уже входит в денежный эффект, и пара удвоила бы
        // приход.
        CorporateAction::PartialRedemption {
            instrument,
            compensation,
            ..
        } => Ok(vec![Leg::principal(account, *instrument, *compensation)]),
        CorporateAction::Redemption {
            instrument,
            custody,
            quantity,
            compensation,
            ..
        } => Ok(vec![
            Leg::principal(account, *instrument, *compensation),
            Leg::security(account, *custody, *instrument, retired(*quantity)?),
        ]),
        CorporateAction::Conversion {
            predecessor,
            successor,
            custody,
            quantity_in,
            quantity_out,
            compensation,
            ..
        } => {
            let mut legs = vec![
                Leg::security(account, *custody, *predecessor, retired(*quantity_in)?),
                Leg::security(account, *custody, *successor, *quantity_out),
            ];
            if let Some(compensation) = compensation {
                legs.push(Leg::cash(account, *compensation));
            }
            Ok(legs)
        }
    }
}

fn offer_legs(account: AccountId, action: &OfferExerciseAction) -> Result<Vec<Leg>, Rejection> {
    match action {
        // Ног нет — и это форма, а не их отсутствие по недосмотру.
        OfferExerciseAction::Submitted { .. } | OfferExerciseAction::Cancelled { .. } => {
            Ok(Vec::new())
        }
        // Выкуп: бумага выбывает за деньги. Ноги `Principal` нет —
        // номинал не возвращается, бумагу выкупают.
        OfferExerciseAction::Settled {
            instrument,
            custody,
            quantity,
            gross,
            fee,
            accrued_interest,
            ..
        } => Ok(vec![
            Leg::cash(account, settlement(*gross, *fee, *accrued_interest)?),
            Leg::security(account, *custody, *instrument, retired(*quantity)?),
        ]),
    }
}

/// Денежный итог выкупа: комиссия уменьшает поступление, накопленный
/// купон увеличивает — та же арифметика, что у продажи.
///
/// Складывается `Money`, а не минорные единицы: сложение денег разных
/// валют обязано быть отказом, а на голых `i64` оно молча пройдёт.
fn settlement(
    gross: Money,
    fee: Option<Money>,
    accrued: Option<Money>,
) -> Result<Money, Rejection> {
    let mut total = gross;
    if let Some(accrued) = accrued {
        total = total.try_add(accrued).map_err(|error| Rejection {
            field: "accrued_interest".into(),
            expected: "сумма в валюте выкупа, дающая представимый итог".into(),
            actual: error.to_string(),
        })?;
    }
    if let Some(fee) = fee {
        total = total.try_sub(fee).map_err(|error| Rejection {
            field: "fee".into(),
            expected: "сумма в валюте выкупа, дающая представимый итог".into(),
            actual: error.to_string(),
        })?;
    }
    Ok(total)
}

/// Количество выбывающей бумаги. Клиент присылает положительное —
/// знак ставит приёмка.
///
/// Отказ пробрасывается, хотя ни одно `Decimal` его сегодня не вызывает
/// (`checked_neg` — это `0 - self`, и у `Decimal` результат представим
/// всегда): `unwrap` здесь утверждал бы, что так будет и впредь, о чём
/// эта функция знать не может. Тот же приём, что в `operation.rs`
/// у продажи.
fn retired(quantity: Quantity) -> Result<Quantity, Rejection> {
    quantity
        .0
        .checked_neg()
        .map(Quantity)
        .map_err(|error| Rejection {
            field: "quantity".into(),
            expected: "представимое количество".into(),
            actual: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::event::corporate_action::{
        BasisTransferRule, CorporateAction, FractionalTreatment,
    };
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::offer::{OfferExerciseAction, OfferSubmissionId, OfferWindowId};
    use iaam_core::ids::{AccountId, CustodyId, InstrumentId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor, Quantity};
    use iaam_core::numeric::decimal::Dec;
    use rust_decimal::Decimal;
    use time::macros::date;

    fn dec(text: &str) -> Dec {
        Dec::new(Decimal::from_str_exact(text).unwrap())
    }

    fn qty(text: &str) -> Quantity {
        Quantity(dec(text))
    }

    fn rub(minor: i64) -> Money {
        Money::new(PostedMinor::new(minor), CurrencyCode::Rub)
    }

    fn context() -> NormalizationContext {
        NormalizationContext {
            owner: OwnerId::new_random(),
            source: SourceId::new_random(),
        }
    }

    fn submitted(fact: JournalFact) -> SubmittedJournalEvent {
        SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact,
            idempotency_key: None,
            source_operation_id: None,
        }
    }

    fn partial_redemption() -> CorporateAction {
        CorporateAction::PartialRedemption {
            instrument: InstrumentId::new_random(),
            custody: CustodyId::new_random(),
            quantity: qty("10"),
            principal_returned_per_unit: PerUnitAmount::new(dec("100"), CurrencyCode::Rub),
            compensation: rub(100_000),
            effective_date: date!(2026 - 05 - 20),
            record_date: Some(date!(2026 - 05 - 18)),
            grounds: None,
        }
    }

    /// Форму события проверяет ядро, а не приёмка: нормализатор обязан
    /// строить ровно те ноги, которых ядро ждёт, иначе запись отклонят
    /// уже после — на общем шве.
    fn normalized_and_valid(fact: JournalFact) -> iaam_core::event::Event {
        let event = normalize_journal_event(&submitted(fact), context())
            .expect("нормализация обязана пройти")
            .event;
        event
            .validate_structure()
            .expect("ноги обязаны совпасть с формой, которую ждёт ядро");
        event
    }

    #[test]
    fn an_amortisation_pays_money_and_leaves_the_quantity_alone() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(event.legs.len(), 1, "{:?}", event.legs);
        assert!(
            matches!(event.kind, EventKind::CorporateAction { .. }),
            "{:?}",
            event.kind
        );
    }

    /// Ровно то поле, ради которого заведён отдельный вход: у операций
    /// его выразить нечем.
    #[test]
    fn the_record_date_reaches_the_entitlement_date() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(
            event.dates.entitlement.map(|day| day.0),
            Some(date!(2026 - 05 - 18)),
            "дата фиксации реестра обязана доехать до события"
        );
    }

    #[test]
    fn a_corporate_action_is_dated_by_the_day_it_takes_effect() {
        let event = normalized_and_valid(JournalFact::CorporateAction(partial_redemption()));
        assert_eq!(
            event.dates.effective_date(),
            Some(date!(2026 - 05 - 20)),
            "без даты событие не попадёт ни в один период"
        );
    }

    #[test]
    fn a_redemption_retires_the_security() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Redemption {
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: qty("10"),
                principal_returned_per_unit: PerUnitAmount::new(dec("1000"), CurrencyCode::Rub),
                compensation: rub(1_000_000),
                effective_date: date!(2026 - 06 - 01),
                record_date: None,
                grounds: None,
            }));
        assert_eq!(event.legs.len(), 2, "{:?}", event.legs);
    }

    #[test]
    fn a_conversion_swaps_the_predecessor_for_the_successor() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Conversion {
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                ratio: dec("1"),
                quantity_in: qty("10"),
                quantity_out: qty("10"),
                fractional: FractionalTreatment::NotApplicable,
                compensation: None,
                effective_date: date!(2026 - 07 - 01),
                record_date: None,
                grounds: None,
                basis_transfer: BasisTransferRule::CarryOver,
            }));
        assert_eq!(event.legs.len(), 2, "{:?}", event.legs);
    }

    #[test]
    fn a_cash_compensated_fraction_adds_a_cash_leg() {
        let event =
            normalized_and_valid(JournalFact::CorporateAction(CorporateAction::Conversion {
                predecessor: InstrumentId::new_random(),
                successor: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                ratio: dec("1.5"),
                quantity_in: qty("11"),
                quantity_out: qty("16"),
                fractional: FractionalTreatment::CashCompensated,
                compensation: Some(rub(5_000)),
                effective_date: date!(2026 - 07 - 01),
                record_date: None,
                grounds: None,
                basis_transfer: BasisTransferRule::CarryOver,
            }));
        assert_eq!(event.legs.len(), 3, "{:?}", event.legs);
    }

    /// Подача заявки ничего не двигает — и отсутствие ног проверяется
    /// наравне с их наличием.
    #[test]
    fn an_offer_application_moves_neither_money_nor_securities() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Submitted {
                submission: OfferSubmissionId::new_random(),
                window: OfferWindowId::new_random(),
                instrument: InstrumentId::new_random(),
                quantity: qty("5"),
            },
            day: date!(2026 - 04 - 10),
        });
        assert!(event.legs.is_empty(), "{:?}", event.legs);
        assert_eq!(event.dates.effective_date(), Some(date!(2026 - 04 - 10)));
    }

    #[test]
    fn a_cancelled_application_is_a_fact_of_its_own() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Cancelled {
                submission: OfferSubmissionId::new_random(),
                quantity: qty("5"),
            },
            day: date!(2026 - 04 - 12),
        });
        assert!(event.legs.is_empty(), "{:?}", event.legs);
    }

    /// Расчёт по оферте — это выбытие бумаги за деньги: комиссия
    /// уменьшает поступление, накопленный купон увеличивает. Знак
    /// количества ставит приёмка, а не клиент.
    #[test]
    fn a_settled_offer_pays_gross_less_fee_plus_accrued_interest() {
        let event = normalized_and_valid(JournalFact::OfferExercise {
            action: OfferExerciseAction::Settled {
                submission: OfferSubmissionId::new_random(),
                instrument: InstrumentId::new_random(),
                custody: CustodyId::new_random(),
                quantity: qty("5"),
                gross: rub(500_000),
                fee: Some(rub(1_000)),
                accrued_interest: Some(rub(2_000)),
            },
            day: date!(2026 - 04 - 20),
        });
        let cash = event
            .legs
            .iter()
            .find_map(iaam_core::event::leg::Leg::cash_effect)
            .expect("расчёт обязан двигать деньги");
        assert_eq!(cash.amount().raw(), 501_000, "500000 - 1000 + 2000");
        // Без даты расчёт не попадёт ни в один период: деньги пришли
        // и бумага выбыла в никуда.
        assert_eq!(event.dates.effective_date(), Some(date!(2026 - 04 - 20)));
    }

    /// Отпечаток обязан РАЗЛИЧАТЬ факты, а не только совпадать сам
    /// с собой: постоянная каноническая форма даёт один отпечаток всему
    /// на свете, и дедупликация объявит дубликатом что угодно (§10.6).
    #[test]
    fn two_different_facts_get_two_different_fingerprints() {
        let account = AccountId::new_random();
        let one = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(partial_redemption()),
            idempotency_key: None,
            source_operation_id: None,
        };
        let mut other = partial_redemption();
        if let CorporateAction::PartialRedemption { compensation, .. } = &mut other {
            *compensation = rub(200_000);
        }
        let two = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(other),
            idempotency_key: None,
            source_operation_id: None,
        };
        assert_ne!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two),
            "две разные выплаты обязаны дать разные отпечатки"
        );
    }

    /// Счёт входит в отпечаток: тот же факт на другом счёте — другой
    /// факт, и слипаться они не должны.
    #[test]
    fn the_same_fact_on_another_account_is_another_fingerprint() {
        let fact = partial_redemption();
        let one = SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact: JournalFact::CorporateAction(fact.clone()),
            idempotency_key: None,
            source_operation_id: None,
        };
        let two = SubmittedJournalEvent {
            account: AccountId::new_random(),
            fact: JournalFact::CorporateAction(fact),
            idempotency_key: None,
            source_operation_id: None,
        };
        assert_ne!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two)
        );
    }

    /// Нулевая компенсация — не «амортизация на ноль», а брак источника.
    /// Отказ обязан случиться до записи: журнал append-only.
    #[test]
    fn a_zero_compensation_is_refused_and_never_becomes_cash() {
        let event = normalize_journal_event(
            &submitted(JournalFact::CorporateAction(
                CorporateAction::PartialRedemption {
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("10"),
                    principal_returned_per_unit: PerUnitAmount::new(dec("0"), CurrencyCode::Rub),
                    compensation: rub(0),
                    effective_date: date!(2026 - 05 - 20),
                    record_date: None,
                    grounds: None,
                },
            )),
            context(),
        )
        .expect("нормализация формы не проверяет")
        .event;
        assert!(
            event.validate_structure().is_err(),
            "нулевая выплата обязана быть отклонена"
        );
    }

    /// Комиссия в чужой валюте — не «почти правильный» выкуп: сложив
    /// её с суммой, приёмка записала бы поступление, которого не было.
    #[test]
    fn a_fee_in_another_currency_is_refused_instead_of_being_added() {
        let rejection = normalize_journal_event(
            &submitted(JournalFact::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("5"),
                    gross: rub(500_000),
                    fee: Some(Money::new(PostedMinor::new(1_000), CurrencyCode::Usd)),
                    accrued_interest: None,
                },
                day: date!(2026 - 04 - 20),
            }),
            context(),
        )
        .expect_err("выкуп с комиссией в чужой валюте обязан быть отклонён");
        assert_eq!(rejection.field, "fee");
    }

    #[test]
    fn accrued_interest_in_another_currency_is_refused_too() {
        let rejection = normalize_journal_event(
            &submitted(JournalFact::OfferExercise {
                action: OfferExerciseAction::Settled {
                    submission: OfferSubmissionId::new_random(),
                    instrument: InstrumentId::new_random(),
                    custody: CustodyId::new_random(),
                    quantity: qty("5"),
                    gross: rub(500_000),
                    fee: None,
                    accrued_interest: Some(Money::new(PostedMinor::new(2_000), CurrencyCode::Usd)),
                },
                day: date!(2026 - 04 - 20),
            }),
            context(),
        )
        .expect_err("накопленный купон в чужой валюте обязан быть отклонён");
        assert_eq!(rejection.field, "accrued_interest");
    }

    /// Идентификатор факта в источнике доезжает до происхождения: без
    /// него сверку с выгрузкой брокера не на чем строить (§4.1).
    #[test]
    fn the_source_identifier_reaches_the_provenance() {
        let event = normalize_journal_event(
            &SubmittedJournalEvent {
                account: AccountId::new_random(),
                fact: JournalFact::CorporateAction(partial_redemption()),
                idempotency_key: None,
                source_operation_id: Some("амортизация-7".into()),
            },
            context(),
        )
        .expect("нормализация обязана пройти")
        .event;
        assert_eq!(
            event.provenance.source_operation_id(),
            Some("амортизация-7")
        );
    }

    /// Отпечаток называет факт, а не подачу: тот же факт с другим ключом
    /// идемпотентности обязан дать тот же отпечаток (§10.6).
    #[test]
    fn the_fingerprint_names_the_fact_not_the_submission() {
        let fact = partial_redemption();
        let account = AccountId::new_random();
        let one = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(fact.clone()),
            idempotency_key: Some("первый".into()),
            source_operation_id: None,
        };
        let two = SubmittedJournalEvent {
            account,
            fact: JournalFact::CorporateAction(fact),
            idempotency_key: Some("второй".into()),
            source_operation_id: Some("внешний-1".into()),
        };
        assert_eq!(
            crate::dedup::fingerprint_journal_event(&one),
            crate::dedup::fingerprint_journal_event(&two)
        );
    }
}
