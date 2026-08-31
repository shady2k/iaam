//! Corrections (§4.8): reversal plus replacement.
//!
//! The journal is append-only, so a correction does not erase the original event;
//! it adds a new one with a reference. The projection is built from the effective
//! set computed by [`resolve`].

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use super::{Event, Relation};
use crate::ids::EventId;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrectionError {
    // The field is NOT named `source`: thiserror treats that name as
    // `Error::source` and requires `std::error::Error`, which the identifier
    // neither implements nor should implement.
    #[error("event {correction:?} references non-existent {target:?}")]
    DanglingTarget {
        correction: EventId,
        target: EventId,
    },
    #[error("event {target:?} is replaced by more than one event: {first:?} and {second:?}")]
    ConflictingReplacements {
        target: EventId,
        first: EventId,
        second: EventId,
    },
    #[error("event {id:?} occurs more than once in the slice")]
    DuplicateEvent { id: EventId },
}

/// Effective event set.
///
/// Returns events sorted by [`crate::dates::EffectiveOrder`], excluding
/// reversed and replaced events. The result **does not depend** on input order:
/// an ordered map is used internally, and conflicts are errors rather than a
/// reason to choose the “last” event.
pub fn resolve(events: &[Event]) -> Result<Vec<&Event>, CorrectionError> {
    // 1. Index by identifier, checking for duplicates.
    let mut by_id: BTreeMap<EventId, &Event> = BTreeMap::new();
    for e in events {
        if by_id.insert(e.id, e).is_some() {
            return Err(CorrectionError::DuplicateEvent { id: e.id });
        }
    }

    // 2. Collect reversed and replaced targets.
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
                    // Deterministic message order: lower identifier first, so
                    // the error text does not depend on import order.
                    //
                    // `min`/`max`, not `if existing < e.id`: strictness is
                    // unobservable here — equal identifiers were rejected as
                    // duplicates above — so neither `<` nor `<=` is tested or
                    // testable.
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

    // 3. An effective event is neither reversed nor replaced, and is not itself
    //    a reversal event.
    let mut effective: Vec<&Event> = events
        .iter()
        .filter(|e| !reversed.contains(&e.id))
        .filter(|e| !replaced_by.contains_key(&e.id))
        .filter(|e| !matches!(e.relation, Relation::Reversal { .. }))
        .collect();

    // Source times order known moments first; raw hashes reproduce equal-time ties.
    effective.sort_by(|left, right| crate::event::compare_for_replay(left, right));

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

    /// An event with a PRESET identifier and order.
    ///
    /// Random identifiers work where only event presence is checked. Where
    /// order itself is checked, they do not: identifier tie-breaking would make
    /// the expected sequence depend on chance.
    fn event_at(id: u128, day: u8, sequence: u32, relation: Relation) -> Event {
        let mut event = sample_event_with(sequence, relation);
        event.id = EventId(Uuid::from_u128(id));
        let date = match day {
            1 => date!(2026 - 03 - 01),
            2 => date!(2026 - 03 - 02),
            _ => panic!("tests use only two days"),
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

    /// All permutations of the slice. Recursion on the head position.
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
        assert!(out.is_empty(), "reversed event is not effective");
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
        assert_eq!(
            out[0].id, replacement.id,
            "the replacement applies, not the original"
        );
        // The append-only journal retains the original record; it merely stops
        // being effective.
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
            "import order must not affect the result"
        );
    }

    #[test]
    fn every_permutation_of_a_mixed_journal_gives_the_same_result() {
        // Two permutations prove nothing: stable sorting without tie-breaking
        // survives them and fails on the third. This journal has every relation
        // kind, an order tie, and a second day; all 720 permutations are checked.
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
        assert_eq!(all.len(), 720, "six elements have exactly 720 permutations");

        // Derived from the rules, not program output:
        // replacement (6) displaces 1 and is first (day 1, sequence 0);
        // 2 and 3 tie on order and are resolved by identifier;
        // 4 is reversed, 5 reverses itself — neither is effective.
        let expected = uuids(&[6, 2, 3]);
        for (n, permutation) in all.iter().enumerate() {
            let out = resolve(permutation).unwrap();
            assert_eq!(
                ids(&out),
                expected,
                "permutation {n} produced a different result"
            );
        }
    }

    #[test]
    fn effective_order_beats_identifier() {
        // A lower identifier with a later order comes AFTER.
        // Without this test, a mutant comparing identifiers only would survive:
        // the permutation test still passes.
        let later = event_at(1, 1, 9, Relation::None);
        let earlier = event_at(2, 1, 1, Relation::None);
        let journal = [later, earlier];
        let out = resolve(&journal).unwrap();
        assert_eq!(ids(&out), uuids(&[2, 1]));
    }

    #[test]
    fn later_date_sorts_after_earlier_date() {
        // Order is the (date, sequence) pair, not sequence alone.
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
        assert_eq!(ids(&out), uuids(&[1, 2]), "ties are resolved by identifier");
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
        assert_eq!(forward, backward, "conflict described identically");
        assert_eq!(forward.to_string(), backward.to_string());

        let CorrectionError::ConflictingReplacements {
            target,
            first,
            second,
        } = forward
        else {
            panic!("expected replacement conflict");
        };
        assert_eq!(target.inner(), Uuid::from_u128(1));
        assert_eq!(first.inner(), Uuid::from_u128(2), "lower identifier first");
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
        // Separate test: target validation for reversals and replacements lives
        // in different branches, and a mutant removes them one at a time.
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
