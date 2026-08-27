//! Свойства с указанием области применимости (§15.3).
//!
//! Каждое свойство сопровождается оговоркой о том, где оно выполняется.
//! Свойства без области — источник ложных падений, на которые проще
//! всего ответить ослаблением генератора до тавтологии.
//!
//! **Намеренно отсутствуют** и не должны быть добавлены:
//! - склейка периодов для XIRR: IRR не цепляется, свойства нет;
//! - масштабирование всех сумм при включённых налогах: прогрессивная
//!   шкала, пороги и минимальные комиссии его нарушают;
//! - сдвиг дат при налоговых правилах: меняются база начисления дней,
//!   налоговый год и ЛДВ.

use iaam_core::dates::TradeDate;
use iaam_core::ids::InstrumentId;
use iaam_core::instrument::AliasInterval;
use iaam_core::money::{CurrencyCode, Money, PostedMinor, Quantity};
use iaam_core::numeric::decimal::Dec;
use iaam_core::rules::lot_disposal::{
    DisposalInput, FifoV1, Lot, LotDisposalRule, LotId, PrincipalState,
};
use proptest::prelude::*;
use rust_decimal::Decimal;
use time::macros::date;

fn lot_strategy() -> impl Strategy<Value = (i64, i64)> {
    // Количество 1..=1000, стоимость 1..=100_000_000 минорных единиц.
    (1_i64..=1_000, 1_i64..=100_000_000)
}

/// Ничья при разнесении округляется к чётному, и стоимость сохраняется.
/// Детерминированный аналог свойства: `proptest` до этого случая
/// может и не добраться.
#[test]
fn tie_rounding_preserves_total_basis() {
    let instrument = InstrumentId::new_random();
    let lots = vec![Lot {
        id: LotId::new_random(),
        instrument,
        acquired: None,
        quantity: Quantity(Dec::new(Decimal::from(2))),
        cost_basis: Money::new(PostedMinor::new(5), CurrencyCode::Rub),
        principal: PrincipalState::Unknown,
    }];

    let out = FifoV1
        .apply(&DisposalInput {
            lots,
            quantity: Quantity(Dec::new(Decimal::from(1))),
        })
        .expect("одна из двух штук доступна");

    // 5 * 1 / 2 = 2,5 — ничья, округляется к чётному, то есть к 2.
    assert_eq!(out.basis_released.amount().raw(), 2);
    assert_eq!(out.remaining[0].cost_basis.amount().raw(), 3);
    assert_eq!(
        out.basis_released.amount().raw() + out.remaining[0].cost_basis.amount().raw(),
        5,
        "стоимость лота обязана сохраниться"
    );
}

proptest! {
    /// Область: любой набор лотов одной валюты, любое допустимое количество.
    /// Инвариант точный — округление разносится так, что суммарная
    /// стоимость лота сохраняется (§6.6).
    ///
    /// Чего свойство **не** ловит: ошибку в самом разнесении. Невыбывшая
    /// часть считается как `cost_basis - taken`, то есть от того же значения,
    /// которое вернул `split_basis`; их сумма равна исходной стоимости при
    /// любом его результате. Проверено: `split_basis`, возвращающий
    /// `value + 1`, оставляет свойство зелёным на 200 000 случаев.
    /// Величину разнесения проверяет `tie_rounding_preserves_total_basis`
    /// и модульные тесты `rules::lot_disposal`. Здесь проверяется другое:
    /// что ни один лот не потерян и не учтён дважды при переходе из
    /// `lots` в `disposed` и `remaining`.
    #[test]
    fn released_plus_remaining_equals_original_basis(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: Some(TradeDate(date!(2026 - 01 - 01))),
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                principal: PrincipalState::Unknown,
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let total_basis: i64 = raw_lots.iter().map(|(_, b)| *b).sum();
        // Целочисленное деление: доля не превышает 100, поэтому результат
        // никогда не больше доступного количества.
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
            .expect("количество в пределах доступного");

        let remaining_basis: i64 =
            out.remaining.iter().map(|l| l.cost_basis.amount().raw()).sum();

        prop_assert_eq!(
            out.basis_released.amount().raw() + remaining_basis,
            total_basis,
            "списанная и оставшаяся стоимость обязаны в сумме давать исходную"
        );
    }

    /// Область: любое допустимое количество. Списанное количество
    /// равно запрошенному — ни больше, ни меньше.
    #[test]
    fn disposed_quantity_equals_requested(
        raw_lots in prop::collection::vec(lot_strategy(), 1..8),
        sell_fraction in 0_u32..=100,
    ) {
        let instrument = InstrumentId::new_random();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                principal: PrincipalState::Unknown,
            })
            .collect();

        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let sell_qty = total_qty * i64::from(sell_fraction) / 100;

        let out = FifoV1
            .apply(&DisposalInput {
                lots,
                quantity: Quantity(Dec::new(Decimal::from(sell_qty))),
            })
            .expect("количество в пределах доступного");

        let disposed: Decimal = out.disposed.iter().map(|d| d.quantity.0.inner()).sum();
        prop_assert_eq!(disposed, Decimal::from(sell_qty));
    }

    /// Область: любые количества сверх доступного.
    /// Отказ, а не отрицательный остаток.
    #[test]
    fn overselling_always_errors(
        raw_lots in prop::collection::vec(lot_strategy(), 1..5),
        excess in 1_i64..=1_000,
    ) {
        let instrument = InstrumentId::new_random();
        let total_qty: i64 = raw_lots.iter().map(|(q, _)| *q).sum();
        let lots: Vec<Lot> = raw_lots
            .iter()
            .map(|(q, b)| Lot {
                id: LotId::new_random(),
                instrument,
                acquired: None,
                quantity: Quantity(Dec::new(Decimal::from(*q))),
                cost_basis: Money::new(PostedMinor::new(*b), CurrencyCode::Rub),
                principal: PrincipalState::Unknown,
            })
            .collect();

        let out = FifoV1.apply(&DisposalInput {
            lots,
            quantity: Quantity(Dec::new(Decimal::from(total_qty + excess))),
        });
        prop_assert!(out.is_err());
    }
}
mod projection_properties {
    use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
    use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::projection::{ProjectionContext, project};
    use iaam_core::rules::{LotRuleVersion, RuleRegistry};
    use proptest::prelude::*;
    use time::macros::date;

    fn deposit(account: AccountId, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2025 - 01 - 01) + time::Duration::days(i64::from(sequence));
        Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner: OwnerId::new_random(),
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"e".repeat(64)).unwrap(),
                ParserVersion("prop/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    proptest! {
        /// Область: всегда (§4.8). Порядок задаёт `EffectiveOrder`,
        /// а не порядок загрузки файлов.
        #[test]
        fn import_order_never_changes_the_projection(
            amounts in prop::collection::vec(1_i64..1_000_000, 1..12),
            rotation in 0_usize..12,
        ) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let events: Vec<Event> = amounts
                .iter()
                .enumerate()
                .map(|(i, minor)| {
                    let index = u32::try_from(i).unwrap_or(u32::MAX);
                    deposit(account, index + 1, *minor)
                })
                .collect();

            let mut rotated = events.clone();
            let shift = rotation % events.len().max(1);
            rotated.rotate_left(shift);

            prop_assert_eq!(
                project(&events, &ctx).unwrap().snapshot().fingerprint(),
                project(&rotated, &ctx).unwrap().snapshot().fingerprint()
            );
        }

        /// Область: всегда. Сторно вместе с исходным событием не оставляют
        /// следа ни в остатках, ни в потоках.
        #[test]
        fn an_event_and_its_reversal_leave_no_trace(minor in 1_i64..1_000_000) {
            let account = AccountId::new_random();
            let contour = ContourDefinition::new(
                ContourId::new_random(),
                ContourVersion(1),
                [account],
            );
            let rules = RuleRegistry::with_defaults();
            let ctx = ProjectionContext {
                contour: &contour,
                rules: &rules,
                lot_rule: LotRuleVersion(1),
            };

            let original = deposit(account, 1, minor);
            let mut reversal = deposit(account, 2, minor);
            reversal.relation = Relation::Reversal { target: original.id };

            let projection = project(&[original, reversal], &ctx).unwrap();
            prop_assert!(projection.state().flows().external().is_empty());
            prop_assert_eq!(
                projection.state().balances().cash(account, CurrencyCode::Rub),
                None
            );
        }
    }
}

proptest! {
    /// Разбор кода не выдумывает род: любая строка, не совпадающая
    /// с кодом варианта, обязана дать None.
    #[test]
    fn an_arbitrary_string_is_never_mistaken_for_a_kind(text in r"\PC{0,16}") {
        let parsed = iaam_core::instrument::InstrumentKind::from_code(&text);
        let expected = iaam_core::instrument::InstrumentKind::ALL
            .into_iter()
            .find(|kind| kind.code() == text);
        prop_assert_eq!(parsed, expected);
    }
}

proptest! {
    /// Из непересекающихся интервалов любую дату покрывает не более
    /// одного.
    ///
    /// Это математическое содержание свойства однозначности резолвинга
    /// (E3.1): резолвер обязан вернуть один инструмент или ни одного,
    /// но никогда двух. Проверяется здесь, а не в `iaam-store`, потому
    /// что база к утверждению отношения не имеет: однозначность даёт
    /// геометрия полуинтервалов, а хранилище лишь обязано ей
    /// пользоваться. Что оно ею действительно пользуется, проверяет
    /// граничный тест `a_code_never_resolves_to_two_instruments`
    /// в `crates/iaam-store/tests/instrument_directory.rs`.
    ///
    /// **Область применимости.** Интервалы строятся непересекающимися
    /// по построению — попарно из отсортированных различных границ,
    /// с зазорами между парами. Пересекающиеся интервалы свойству не
    /// подчиняются, и это не дефект свойства: их запрещает триггер
    /// `instrument_aliases_do_not_overlap` в схеме, а не арифметика.
    #[test]
    fn at_most_one_of_several_disjoint_intervals_covers_any_day(
        bounds in prop::collection::vec(0_i64..3_000, 2..12),
        probe in -100_i64..3_100,
    ) {
        let origin = date!(2020 - 01 - 01);
        let day = |offset: i64| {
            origin
                .checked_add(time::Duration::days(offset))
                .expect("дата в пределах календаря")
        };

        let mut sorted = bounds;
        sorted.sort_unstable();

        // Цепочка СМЕЖНЫХ интервалов: [b0, b1), [b1, b2), [b2, b3), …
        //
        // Именно так выглядит история псевдонима: смена ISIN закрывает
        // старый интервал и открывает новый той же датой. И только на
        // такой форме свойство способно упасть — при включительном
        // конце каждая внутренняя граница попала бы сразу в два
        // интервала. Разрозненные пары с зазорами здесь не годятся:
        // стык между ними приходилось бы ждать от случайного
        // совпадения двух чисел, то есть практически никогда, и
        // свойство молча выродилось бы в тавтологию.
        //
        // Вырожденные звенья (b_i == b_{i+1}) отбрасываются: пустой
        // интервал запрещён проверкой CHECK в схеме и покрывает пустое
        // множество.
        let intervals: Vec<AliasInterval> = sorted
            .windows(2)
            .filter(|pair| pair[0] < pair[1])
            .map(|pair| AliasInterval {
                valid_from: day(pair[0]),
                valid_to: Some(day(pair[1])),
            })
            .collect();
        prop_assume!(!intervals.is_empty());

        // Проверяются ВСЕ границы цепочки, а не только случайная дата.
        // Случайный пробник попадает ровно в границу примерно в одном
        // случае из трёхсот, поэтому off-by-one он находит от случая
        // к случаю — то есть не находит. Граница же и есть то место,
        // где интервалы могут наложиться.
        let mut probes: Vec<i64> = sorted.clone();
        probes.push(probe);

        for point in probes {
            let covering = intervals
                .iter()
                .filter(|interval| interval.covers(day(point)))
                .count();
            prop_assert!(
                covering <= 1,
                "день {point} покрыт {covering} интервалами из {intervals:?}"
            );
        }
    }
}
