//! Исправления (§4.8): сторнирование плюс замена.
//!
//! Журнал append-only, поэтому исправление не стирает исходное событие,
//! а добавляет новое со ссылкой. Проекция строится по действующему
//! набору, который вычисляет [`resolve`].

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use super::{Event, Relation};
use crate::ids::EventId;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrectionError {
    // Поле НЕ называется `source`: thiserror трактует поле с таким именем
    // как `Error::source` и требует от него реализации `std::error::Error`,
    // которой у идентификатора нет и быть не должно.
    #[error("событие {correction:?} ссылается на несуществующее {target:?}")]
    DanglingTarget {
        correction: EventId,
        target: EventId,
    },
    #[error("событие {target:?} заменяется более чем одним событием: {first:?} и {second:?}")]
    ConflictingReplacements {
        target: EventId,
        first: EventId,
        second: EventId,
    },
    #[error("событие {id:?} встречается в срезе более одного раза")]
    DuplicateEvent { id: EventId },
}

/// Действующий набор событий.
///
/// Возвращает события, отсортированные по [`crate::dates::EffectiveOrder`],
/// с исключёнными сторнированными и заменёнными. Результат **не зависит**
/// от порядка входного среза: внутри используется упорядоченная карта,
/// а конфликты являются ошибкой, а не поводом выбрать «последний».
pub fn resolve(events: &[Event]) -> Result<Vec<&Event>, CorrectionError> {
    // 1. Индекс по идентификатору, с проверкой на дубликаты.
    let mut by_id: BTreeMap<EventId, &Event> = BTreeMap::new();
    for e in events {
        if by_id.insert(e.id, e).is_some() {
            return Err(CorrectionError::DuplicateEvent { id: e.id });
        }
    }

    // 2. Собираем сторнированные и заменённые цели.
    let mut reversed: BTreeSet<EventId> = BTreeSet::new();
    let mut replaced_by: BTreeMap<EventId, EventId> = BTreeMap::new();

    for e in events {
        match e.relation {
            Relation::None => {}
            Relation::Reversal { target } => {
                if !by_id.contains_key(&target) {
                    return Err(CorrectionError::DanglingTarget {
                        correction: e.id,
                        target,
                    });
                }
                reversed.insert(target);
            }
            Relation::Replacement { target } => {
                if !by_id.contains_key(&target) {
                    return Err(CorrectionError::DanglingTarget {
                        correction: e.id,
                        target,
                    });
                }
                if let Some(existing) = replaced_by.insert(target, e.id) {
                    // Детерминированный порядок сообщения: меньший идентификатор первым,
                    // чтобы текст ошибки не зависел от порядка импорта.
                    //
                    // `min`/`max`, а не `if existing < e.id`: строгость сравнения
                    // здесь ненаблюдаема — равные идентификаторы отсеяны проверкой
                    // на дубликаты выше, — поэтому выбор между `<` и `<=` не
                    // проверяется ни одним тестом и быть проверен не может.
                    let (first, second) = (existing.min(e.id), existing.max(e.id));
                    return Err(CorrectionError::ConflictingReplacements {
                        target,
                        first,
                        second,
                    });
                }
            }
        }
    }

    // 3. Действующим является событие, которое не сторнировано,
    //    не заменено, и само не является сторнирующим.
    let mut effective: Vec<&Event> = events
        .iter()
        .filter(|e| !reversed.contains(&e.id))
        .filter(|e| !replaced_by.contains_key(&e.id))
        .filter(|e| !matches!(e.relation, Relation::Reversal { .. }))
        .collect();

    // 4. Устойчивая сортировка: порядок задаётся EffectiveOrder,
    //    ничьи разрешаются идентификатором.
    effective.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    Ok(effective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::EffectiveOrder;
    use crate::event::test_support::{sample_event, sample_event_with};
    use crate::event::{Event, Relation};
    use crate::ids::EventId;
    use time::macros::date;
    use uuid::Uuid;

    /// Событие с ЗАДАННЫМ идентификатором и порядком.
    ///
    /// Случайные идентификаторы годятся там, где проверяется только
    /// наличие события в результате. Там, где проверяется сам порядок,
    /// они не годятся: ничьи разрешаются идентификатором, и ожидаемая
    /// последовательность зависела бы от жребия.
    fn event_at(id: u128, day: u8, sequence: u32, relation: Relation) -> Event {
        let mut event = sample_event_with(sequence, relation);
        event.id = EventId(Uuid::from_u128(id));
        let date = match day {
            1 => date!(2026 - 03 - 01),
            2 => date!(2026 - 03 - 02),
            _ => panic!("в тестах используются только два дня"),
        };
        event.order = EffectiveOrder::new(date, sequence);
        event
    }

    fn ids(events: &[&Event]) -> Vec<Uuid> {
        events.iter().map(|e| e.id.inner()).collect()
    }

    fn uuids(raw: &[u128]) -> Vec<Uuid> {
        raw.iter().map(|n| Uuid::from_u128(*n)).collect()
    }

    /// Все перестановки среза. Рекурсия по позиции головы.
    fn permutations(events: &[Event]) -> Vec<Vec<Event>> {
        if events.len() <= 1 {
            return vec![events.to_vec()];
        }
        let mut out = Vec::new();
        for i in 0..events.len() {
            let mut rest = events.to_vec();
            let head = rest.remove(i);
            for mut tail in permutations(&rest) {
                tail.insert(0, head.clone());
                out.push(tail);
            }
        }
        out
    }

    #[test]
    fn plain_event_is_effective() {
        let e = sample_event(0);
        let journal = [e.clone()];
        let out = resolve(&journal).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, e.id);
    }

    #[test]
    fn reversal_cancels_its_target() {
        let original = sample_event(0);
        let reversal = sample_event_with(
            1,
            Relation::Reversal {
                target: original.id,
            },
        );
        let journal = [original, reversal];
        let out = resolve(&journal).unwrap();
        assert!(out.is_empty(), "сторнированное событие не действует");
    }

    #[test]
    fn replacement_supersedes_its_target() {
        let original = sample_event(0);
        let replacement = sample_event_with(
            1,
            Relation::Replacement {
                target: original.id,
            },
        );
        let journal = [original.clone(), replacement.clone()];
        let out = resolve(&journal).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, replacement.id, "действует замена, не исходное");
        // Журнал append-only: исходная запись из него не исчезает,
        // она лишь перестаёт действовать.
        assert_eq!(journal.len(), 2);
        assert_eq!(journal[0].id, original.id);
    }

    #[test]
    fn replacement_of_a_replacement_leaves_only_the_last() {
        let original = sample_event(0);
        let middle = sample_event_with(
            1,
            Relation::Replacement {
                target: original.id,
            },
        );
        let last = sample_event_with(2, Relation::Replacement { target: middle.id });
        let journal = [original, middle, last.clone()];
        let out = resolve(&journal).unwrap();
        assert_eq!(ids(&out), vec![last.id.inner()]);
    }

    #[test]
    fn result_does_not_depend_on_input_order() {
        let original = sample_event(0);
        let replacement = sample_event_with(
            1,
            Relation::Replacement {
                target: original.id,
            },
        );

        let forward_journal = [original.clone(), replacement.clone()];
        let backward_journal = [replacement, original];
        let forward = resolve(&forward_journal).unwrap();
        let backward = resolve(&backward_journal).unwrap();

        let ids_forward: Vec<_> = forward.iter().map(|e| e.id).collect();
        let ids_backward: Vec<_> = backward.iter().map(|e| e.id).collect();
        assert_eq!(
            ids_forward, ids_backward,
            "порядок импорта не должен влиять"
        );
    }

    #[test]
    fn every_permutation_of_a_mixed_journal_gives_the_same_result() {
        // Две перестановки ничего не доказывают: устойчивая сортировка
        // без разрешения ничьи выдержит их и развалится на третьей.
        // Здесь журнал со всеми видами связи, ничьёй по порядку и вторым
        // днём, и проверяются ВСЕ 720 перестановок.
        let plain_early = event_at(1, 1, 1, Relation::None);
        let tie_low = event_at(2, 1, 2, Relation::None);
        let tie_high = event_at(3, 1, 2, Relation::None);
        let reversed = event_at(4, 2, 0, Relation::None);
        let reversal = event_at(
            5,
            2,
            5,
            Relation::Reversal {
                target: reversed.id,
            },
        );
        let replacement = event_at(
            6,
            1,
            0,
            Relation::Replacement {
                target: plain_early.id,
            },
        );

        let journal = [
            plain_early,
            tie_low,
            tie_high,
            reversed,
            reversal,
            replacement,
        ];
        let all = permutations(&journal);
        assert_eq!(all.len(), 720, "перестановок шести элементов ровно 720");

        // Ожидание выведено из правил, а не из вывода программы:
        // замена (6) вытесняет 1 и стоит первой по порядку (день 1, seq 0);
        // 2 и 3 — ничья по порядку, разрешается идентификатором;
        // 4 сторнировано, 5 само сторнирует — оба не действуют.
        let expected = uuids(&[6, 2, 3]);
        for (n, permutation) in all.iter().enumerate() {
            let out = resolve(permutation).unwrap();
            assert_eq!(
                ids(&out),
                expected,
                "перестановка №{n} дала другой результат"
            );
        }
    }

    #[test]
    fn effective_order_beats_identifier() {
        // Меньший идентификатор при большем порядке идёт ПОСЛЕ.
        // Без этого теста мутант, сравнивающий только идентификаторы,
        // выжил бы: перестановочный тест он проходит.
        let later = event_at(1, 1, 9, Relation::None);
        let earlier = event_at(2, 1, 1, Relation::None);
        let journal = [later, earlier];
        let out = resolve(&journal).unwrap();
        assert_eq!(ids(&out), uuids(&[2, 1]));
    }

    #[test]
    fn later_date_sorts_after_earlier_date() {
        // Порядок задаётся парой (дата, номер), а не одним номером.
        let second_day = event_at(1, 2, 0, Relation::None);
        let first_day = event_at(2, 1, 7, Relation::None);
        let journal = [second_day, first_day];
        let out = resolve(&journal).unwrap();
        assert_eq!(ids(&out), uuids(&[2, 1]));
    }

    #[test]
    fn ties_are_broken_by_identifier() {
        let high = event_at(2, 1, 3, Relation::None);
        let low = event_at(1, 1, 3, Relation::None);
        let journal = [high, low];
        let out = resolve(&journal).unwrap();
        assert_eq!(
            ids(&out),
            uuids(&[1, 2]),
            "ничья разрешается идентификатором"
        );
    }

    #[test]
    fn conflicting_replacements_are_an_error() {
        let original = sample_event(0);
        let first = sample_event_with(
            1,
            Relation::Replacement {
                target: original.id,
            },
        );
        let second = sample_event_with(
            2,
            Relation::Replacement {
                target: original.id,
            },
        );
        assert!(matches!(
            resolve(&[original, first, second]),
            Err(CorrectionError::ConflictingReplacements { .. })
        ));
    }

    #[test]
    fn conflict_report_does_not_depend_on_input_order() {
        let original = event_at(1, 1, 0, Relation::None);
        let low = event_at(
            2,
            1,
            1,
            Relation::Replacement {
                target: original.id,
            },
        );
        let high = event_at(
            3,
            1,
            2,
            Relation::Replacement {
                target: original.id,
            },
        );

        let forward = resolve(&[original.clone(), low.clone(), high.clone()]).unwrap_err();
        let backward = resolve(&[high, low, original]).unwrap_err();
        assert_eq!(forward, backward, "конфликт описан одинаково");
        assert_eq!(forward.to_string(), backward.to_string());

        let CorrectionError::ConflictingReplacements {
            target,
            first,
            second,
        } = forward
        else {
            panic!("ожидался конфликт замен");
        };
        assert_eq!(target.inner(), Uuid::from_u128(1));
        assert_eq!(
            first.inner(),
            Uuid::from_u128(2),
            "меньший идентификатор первым"
        );
        assert_eq!(second.inner(), Uuid::from_u128(3));
    }

    #[test]
    fn dangling_target_is_an_error() {
        let orphan = sample_event_with(
            0,
            Relation::Reversal {
                target: crate::ids::EventId::new_random(),
            },
        );
        assert!(matches!(
            resolve(&[orphan]),
            Err(CorrectionError::DanglingTarget { .. })
        ));
    }

    #[test]
    fn dangling_replacement_target_is_an_error() {
        // Отдельный тест: проверки цели у сторнирования и у замены —
        // разные ветки, и мутант убирает их по одной.
        let missing = EventId::new_random();
        let orphan = sample_event_with(0, Relation::Replacement { target: missing });
        let err = resolve(std::slice::from_ref(&orphan)).unwrap_err();
        assert_eq!(
            err,
            CorrectionError::DanglingTarget {
                correction: orphan.id,
                target: missing,
            }
        );
    }

    #[test]
    fn duplicate_event_is_an_error() {
        let e = sample_event(0);
        let err = resolve(&[e.clone(), e.clone()]).unwrap_err();
        assert_eq!(err, CorrectionError::DuplicateEvent { id: e.id });
    }

    #[test]
    fn empty_journal_has_no_effective_events() {
        assert!(resolve(&[]).unwrap().is_empty());
    }
}
