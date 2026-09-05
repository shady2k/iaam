//! One movement a single document printed on both of its accounts (iaam-3qsq).
//!
//! A statement that covers two of the owner's own accounts prints one movement
//! between them **twice**: a departure on the account it left and an arrival on
//! the account it reached, same day, same amount, opposite signs, under one
//! word of the source's own. Nothing in either row says the two are one
//! movement, and the journal shape for a movement between own accounts carries
//! a leg on **each** account — so a reader that takes both rows at face value
//! records the movement twice and every account moves twice.
//!
//! **This is not [`crate::classification`]'s question and not the import
//! session's transfer pairing.** Transfer pairing relates a row printed by one
//! institution to a row printed by another: two documents, two readings,
//! nothing in common but a shape, and a three-day window because the two banks
//! post on their own schedules. Here there is one document, one reading, one
//! day and one amount, and the two rows are the two halves the source itself
//! printed. Widening the pairing window to cover this would let the
//! cross-institution matcher decide something it must never decide, and
//! narrowing it would break the case it exists for.
//!
//! **Nothing here concludes that two rows are one movement on its own.** The
//! test says which rows *could* be the two halves of one movement; what makes
//! the pair real is an account named on one side — by the source, by the
//! owner's directory, or by the owner's own answer — and the caller supplies
//! that as [`MirrorSide::far_side`]. Two rows that name nobody are a
//! **hypothesis** and are reported as one, so that one decision can be put to
//! the owner instead of two; his answer is what settles it, and any answer that
//! does not name the other row's account leaves the two rows as two rows. That
//! is the refusal the module would be worse than useless without: two unrelated
//! payments of one amount on one day exist, and «no, these are two different
//! things» must remain sayable.
//!
//! **Ambiguity pairs nothing.** A row that could be the far half of movements
//! on two different accounts is matched with neither: which one it is changes
//! what is written, and picking is the fabrication. Several rows that agree on
//! everything *including* the two accounts are a different case and are matched
//! one-to-one, because whichever way they are matched the facts come out the
//! same — see [`mirrored`].
//!
//! **A side that pairs with nothing is reported, and the two ways it can fail
//! to pair are two sentences** (`iaam-0evk`). The pairs alone said nothing about
//! the rows left out of them, and a caller cannot recover the difference: a
//! departure whose arrival this document does not hold looks, from a list of
//! pairs, exactly like a card payment that was never a side of anything. So the
//! reading names them — [`Unpaired::NoCounterpart`] for a row this document
//! holds no other half for, [`Unpaired::Ambiguous`] for the refusal above — and
//! the first of the two is «not in **this** document» and never «nowhere». This
//! module reads one document by construction, a movement between two accounts
//! prints its halves on two accounts, and the far half may be in a statement
//! nobody has brought here or on an account the owner never named. Denying it
//! would be the same fabrication as picking a partner.

use iaam_core::ids::AccountId;
use iaam_core::money::CurrencyCode;
use time::Date;

use crate::classification::Movement;

/// One row of a document, as the mirror test reads it.
///
/// Deliberately **not** an [`crate::observation::ObservedRow`]. The test asks
/// four things of a row — which account, which way, how much, and which day —
/// and a fifth that no row states on its own: which account the reading of it
/// names on the far side. That fifth field is why the caller builds this rather
/// than the module reading rows itself: the far side may come from the source's
/// own counterparty column, from the owner's directory recognising it, or from
/// an answer he gave, and only the caller holds all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorSide {
    /// The row's number within its session, which is what the caller acts on.
    pub row: u32,
    /// The account whose statement printed the row.
    pub account: AccountId,
    /// Which way the money ran on [`Self::account`].
    ///
    /// A row that states no direction is not a side: the test turns on the two
    /// halves running opposite ways, and a row with no direction would match
    /// both. Such a row is left out by the caller rather than given a default
    /// here.
    pub direction: Movement,
    /// The magnitude in minor units, always positive: the direction is carried
    /// by [`Self::direction`], and a sign beside it would be a second and
    /// disagreeable statement of the same thing.
    pub amount_minor: i64,
    pub currency: CurrencyCode,
    /// The day the source posted the row.
    pub date: Date,
    /// The account the reading of this row names on the other side, where
    /// anything names one.
    ///
    /// `None` is not «no far side»: it is «nobody has named one yet», which is
    /// the state of a row still waiting on the owner. A `Some` that names some
    /// third account is what stops a row being paired with the wrong partner —
    /// a row whose far side is asserted to be one account is not half of a
    /// movement to another.
    pub far_side: Option<AccountId>,
}

/// What makes the two rows one movement.
///
/// Three values, and they are three strengths of evidence rather than three
/// shapes of pair. What a caller does with them differs: the first two are the
/// document or the owner speaking, and a fact may be collapsed on them; the
/// third is a shape, and the only thing it may do is put **one** question where
/// there would have been two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorEvidence {
    /// Each row names the other's account on its far side.
    BothSidesNamed,
    /// One row names the other's account, and the other names nobody.
    OneSideNamed,
    /// Neither row names an account, and the two agree on day, currency,
    /// magnitude and opposing direction.
    ///
    /// A hypothesis and never a fact. Two unrelated payments of one amount on
    /// one day have exactly this shape, so a caller that recorded anything on
    /// the strength of it would be inventing the movement the whole module
    /// exists to avoid inventing twice.
    ShapeAlone,
}

impl MirrorEvidence {
    /// Whether anything but the shape of the two rows supports the pair.
    ///
    /// The one predicate a caller needs, so that «may I act on this» is asked
    /// once here rather than matched on at each call site — where the third
    /// value would eventually be folded in with the other two by somebody
    /// writing an exhaustive match in a hurry.
    #[must_use]
    pub const fn is_named(self) -> bool {
        matches!(self, Self::BothSidesNamed | Self::OneSideNamed)
    }

    /// Wire code. One place, so two publishers cannot spell it differently.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BothSidesNamed => "both_sides_named",
            Self::OneSideNamed => "one_side_named",
            Self::ShapeAlone => "shape_alone",
        }
    }
}

/// Two rows of one document that are one movement printed twice.
///
/// The two sides are named by direction and not by which of them records the
/// fact, because that is the caller's decision and it depends on what has been
/// settled: a movement whose outgoing row the owner has answered is recorded
/// from the sending side, and one whose *incoming* row he answered is recorded
/// from the sending side too — by the answer he gave on the arriving row, which
/// already carries both accounts. Naming a «primary» here would fix that choice
/// in the wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mirror {
    /// The row on the account the money left.
    pub outgoing: u32,
    /// The row on the account the money reached.
    pub incoming: u32,
    pub evidence: MirrorEvidence,
}

impl Mirror {
    /// The other row of this pair, if this pair contains that row at all.
    #[must_use]
    pub const fn partner_of(&self, row: u32) -> Option<u32> {
        if self.outgoing == row {
            Some(self.incoming)
        } else if self.incoming == row {
            Some(self.outgoing)
        } else {
            None
        }
    }
}

/// What one reading of a document's rows made of every one of them.
///
/// **The pairs and the sides that found none, because absence published nowhere
/// is a fact this module knows and nobody else can recover** (`iaam-0evk`). A
/// side left out of [`Self::pairs`] used to be indistinguishable, at every
/// surface downstream, from a row that was never leg-shaped at all — so a
/// departure whose arrival this document does not hold was offered the ordinary
/// alternatives, and both of the ordinary answers are wrong for it: naming a far
/// account records a movement whose other half is not there, and «I paid
/// somebody» files a movement between the owner's own accounts as spending.
///
/// Returned together from one pass rather than computed twice, for the reason
/// the caller's own pairing is derived and never stored: two answers to «is this
/// row half of anything» can disagree, and the one place they would disagree is
/// the row nobody paired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirroring {
    /// The rows this reading concluded are two sights of one movement.
    pub pairs: Vec<Mirror>,
    /// The sides it left in no pair, each with which of the two that is, in row
    /// order.
    pub unpaired: Vec<UnpairedSide>,
}

/// One side no pair of this reading holds, and why it holds none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpairedSide {
    /// The row's number within its session, which is what the caller acts on.
    pub row: u32,
    pub reason: Unpaired,
}

/// Why a side of this document is in no pair.
///
/// **Two values, and folding them into one word would publish the wrong thing
/// about both.** They are opposites: one says this document holds nothing that
/// could be the other half, the other says it holds more than one and states
/// nothing that chooses. A caller that told the owner «there is no counterpart
/// here» about an ambiguous row would be denying rows the document printed; one
/// that told him «this could not be decided» about a lone row would send him
/// looking through a statement for something that is not in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unpaired {
    /// No row of this document is available to be the other half of this one.
    ///
    /// Two situations and they are one fact about the row. Nothing in the
    /// document mirrors it at all; or everything that does is already the other
    /// half of another movement — one arrival cannot be the far half of two
    /// departures, so the departure left over is a departure whose counterpart
    /// this document does not hold.
    ///
    /// **It is «not in this document» and never «nowhere».** This module reads
    /// one document by construction, and a movement between two accounts prints
    /// its halves on two accounts: the far half may sit in a statement nobody
    /// has brought here, or on an account the owner never put in his directory.
    /// Neither is visible from here, and neither is denied.
    NoCounterpart,
    /// More than one row could be the other half, and the document states
    /// nothing that chooses between them.
    ///
    /// [`mirrored`]'s existing refusal, reported rather than made silent. The
    /// doubt belongs to the pair and not to whichever side is read first, so a
    /// row whose only candidate is itself undecidable is undecidable too.
    Ambiguous,
}

/// Pair the rows of one document that are one movement seen twice.
///
/// Two sides mirror each other when they are on two different accounts, agree
/// on currency, magnitude and day, run opposite ways, and neither names a far
/// side that is not the other's account.
///
/// # Why the day is exact and not a window
///
/// [`crate::observation::ObservedRow`] values reaching here came out of **one**
/// document, and a document that prints both halves of a movement prints them
/// on the day it posted them. A window would be a claim about two institutions
/// posting on their own schedules, which is the cross-institution matcher's
/// case and not this one, and it would let two genuinely different rows of one
/// statement pair across a weekend for no gain at all.
///
/// # Why ambiguity pairs nothing, and what is not ambiguous
///
/// A side that mirrors rows on **more than one account** is matched with none:
/// which account it went to changes what is written, and there is no evidence
/// in the document to choose. That is the case the refusal is for.
///
/// Several sides that agree on everything *including* the pair of accounts are
/// not that case. Two identical movements between the same two accounts on one
/// day print four rows, two on each side, and every way of matching them yields
/// the same two facts — so they are matched in row order, and the leftover on
/// either side keeps its own fact. Refusing them would double-count exactly the
/// document that stated itself most completely.
#[must_use]
pub fn mirrored(sides: &[MirrorSide]) -> Mirroring {
    let mut pairs = Vec::new();
    for outgoing in sides.iter().filter(|side| side.direction == Movement::Out) {
        // The accounts this side could have moved to. More than one and it is
        // matched with nothing, whatever else is true of it.
        let partners = counterpart_accounts(sides, outgoing);
        let [account] = partners.as_slice() else {
            continue;
        };
        for incoming in sides
            .iter()
            .filter(|side| side.direction == Movement::In && side.account == *account)
            .filter(|incoming| mirrors(outgoing, incoming))
        {
            // The same question asked of the other side. An incoming row that
            // could have come from two accounts is as undecidable as an
            // outgoing one that could have gone to two, and the doubt belongs
            // to the pair rather than to whichever side happens to be read
            // first.
            if counterpart_accounts(sides, incoming).len() != 1 {
                continue;
            }
            pairs.push((outgoing, incoming));
        }
    }

    // One row is one half of one movement. Where several sides agree on
    // everything — the two accounts included — the matching is by row order,
    // which is the document's own order and is what makes two readings of one
    // session produce one answer.
    let mut taken_out: Vec<u32> = Vec::new();
    let mut taken_in: Vec<u32> = Vec::new();
    let mut matched = Vec::new();
    pairs.sort_by_key(|(outgoing, incoming)| (outgoing.row, incoming.row));
    for (outgoing, incoming) in pairs {
        if taken_out.contains(&outgoing.row) || taken_in.contains(&incoming.row) {
            continue;
        }
        taken_out.push(outgoing.row);
        taken_in.push(incoming.row);
        matched.push(Mirror {
            outgoing: outgoing.row,
            incoming: incoming.row,
            evidence: evidence(outgoing, incoming),
        });
    }
    // Every side no pair holds, in row order, each with which of the two
    // reasons it is. Read off the same pass rather than recomputed by a caller:
    // the taken sets above *are* the answer to «did this row find a partner»,
    // and a second reader deriving it from the pairs would have to reconstruct
    // which rows were sides at all — the one thing a list of pairs cannot say.
    let mut unpaired: Vec<UnpairedSide> = sides
        .iter()
        .filter(|side| !taken_out.contains(&side.row) && !taken_in.contains(&side.row))
        .map(|side| UnpairedSide {
            row: side.row,
            reason: why_unpaired(sides, side),
        })
        .collect();
    unpaired.sort_unstable_by_key(|side| side.row);
    Mirroring {
        pairs: matched,
        unpaired,
    }
}

/// Which of the two reasons this side is in no pair.
///
/// **Decided by what the document holds, not by what the pass happened to do
/// with it.** A side is [`Unpaired::Ambiguous`] where it could be the near half
/// of movements on more than one account — the refusal [`mirrored`] already
/// makes — and where the only rows that mirror it are themselves undecidable,
/// because the doubt belongs to the pair and not to whichever side is read
/// first. That second clause is what keeps the two arrivals of one ambiguous
/// departure from being told, each on its own, that this document holds nothing
/// for them: it holds the departure, and what it does not hold is a way to
/// choose.
///
/// Everything else is [`Unpaired::NoCounterpart`], the leftover of several
/// identical sides included. One arrival cannot be the other half of two
/// departures, so once the pairing has spent it the departure left over is a
/// departure this document holds no counterpart for — which is the same
/// sentence as a departure nothing mirrored at all, because it is the same fact
/// about the row.
fn why_unpaired(sides: &[MirrorSide], side: &MirrorSide) -> Unpaired {
    if counterpart_accounts(sides, side).len() > 1
        || counterparts(sides, side).any(|other| counterpart_accounts(sides, other).len() > 1)
    {
        Unpaired::Ambiguous
    } else {
        Unpaired::NoCounterpart
    }
}

/// The accounts a side could be the near half of a movement to or from.
///
/// A set rather than a count, because the refusal is about *which* account and
/// not about how many rows: two arriving rows on one account are one candidate
/// account, and matching the departure with either of them writes the same
/// fact.
fn counterpart_accounts(sides: &[MirrorSide], side: &MirrorSide) -> Vec<AccountId> {
    let mut accounts: Vec<AccountId> = counterparts(sides, side)
        .map(|other| other.account)
        .collect();
    accounts.sort_unstable();
    accounts.dedup();
    accounts
}

/// The rows of this document that could be the other half of this one.
///
/// The direction is read from the side being asked about rather than fixed,
/// because [`mirrors`] takes the departure first and the question is asked from
/// both sides.
fn counterparts<'a>(
    sides: &'a [MirrorSide],
    side: &'a MirrorSide,
) -> impl Iterator<Item = &'a MirrorSide> {
    sides
        .iter()
        .filter(move |other| other.direction != side.direction)
        .filter(move |other| match side.direction {
            Movement::Out => mirrors(side, other),
            Movement::In => mirrors(other, side),
        })
}

/// Whether these two sides are the two halves of one movement.
///
/// The far-side test is «names nobody, or names the other», in both directions.
/// A row whose far side the source asserted to be one account is not half of a
/// movement to another, and reading that assertion as a mere hint would make
/// the one field a source fills in about its own internal transfers worth less
/// than the shape of the row it is printed on.
fn mirrors(outgoing: &MirrorSide, incoming: &MirrorSide) -> bool {
    outgoing.account != incoming.account
        && outgoing.currency == incoming.currency
        && outgoing.amount_minor == incoming.amount_minor
        && outgoing.amount_minor > 0
        && outgoing.date == incoming.date
        && outgoing
            .far_side
            .is_none_or(|named| named == incoming.account)
        && incoming
            .far_side
            .is_none_or(|named| named == outgoing.account)
}

const fn evidence(outgoing: &MirrorSide, incoming: &MirrorSide) -> MirrorEvidence {
    match (outgoing.far_side.is_some(), incoming.far_side.is_some()) {
        (true, true) => MirrorEvidence::BothSidesNamed,
        (true, false) | (false, true) => MirrorEvidence::OneSideNamed,
        (false, false) => MirrorEvidence::ShapeAlone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn side(row: u32, account: AccountId, direction: Movement, amount_minor: i64) -> MirrorSide {
        MirrorSide {
            row,
            account,
            direction,
            amount_minor,
            currency: CurrencyCode::Rub,
            date: date!(2025 - 04 - 10),
            far_side: None,
        }
    }

    #[test]
    fn a_departure_and_an_arrival_of_one_amount_on_one_day_are_one_movement() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mirrors = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, savings, Movement::In, 250_000),
        ])
        .pairs;
        assert_eq!(
            mirrors,
            vec![Mirror {
                outgoing: 1,
                incoming: 2,
                evidence: MirrorEvidence::ShapeAlone,
            }]
        );
    }

    #[test]
    fn two_rows_that_name_each_other_are_paired_on_the_documents_own_word() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mut out = side(1, main, Movement::Out, 250_000);
        out.far_side = Some(savings);
        let mut into = side(2, savings, Movement::In, 250_000);
        into.far_side = Some(main);
        let mirrors = mirrored(&[out, into]).pairs;
        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].evidence, MirrorEvidence::BothSidesNamed);
        assert!(mirrors[0].evidence.is_named());
    }

    #[test]
    fn a_row_whose_far_side_is_named_is_not_paired_with_a_row_on_another_account() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let reserve = AccountId::new_random();
        let mut out = side(1, main, Movement::Out, 250_000);
        out.far_side = Some(reserve);
        assert!(
            mirrored(&[out, side(2, savings, Movement::In, 250_000)])
                .pairs
                .is_empty()
        );
    }

    #[test]
    fn a_departure_that_could_have_gone_to_two_accounts_is_paired_with_neither() {
        // The document says one amount left on one day and the same amount
        // arrived at two places. Which one it went to changes what is written,
        // and nothing in the rows decides it.
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let reserve = AccountId::new_random();
        assert!(
            mirrored(&[
                side(1, main, Movement::Out, 250_000),
                side(2, savings, Movement::In, 250_000),
                side(3, reserve, Movement::In, 250_000),
            ])
            .pairs
            .is_empty()
        );
    }

    #[test]
    fn two_identical_movements_between_one_pair_of_accounts_are_two_pairs() {
        // Every way of matching these four rows yields the same two facts, so
        // refusing them would double-count the document that stated itself most
        // completely.
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mirrors = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, main, Movement::Out, 250_000),
            side(3, savings, Movement::In, 250_000),
            side(4, savings, Movement::In, 250_000),
        ])
        .pairs;
        assert_eq!(
            mirrors
                .iter()
                .map(|mirror| (mirror.outgoing, mirror.incoming))
                .collect::<Vec<_>>(),
            vec![(1, 3), (2, 4)]
        );
    }

    #[test]
    fn a_departure_with_no_arrival_to_match_keeps_its_own_fact() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mirrors = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, main, Movement::Out, 250_000),
            side(3, savings, Movement::In, 250_000),
        ])
        .pairs;
        assert_eq!(mirrors.len(), 1);
        assert_eq!((mirrors[0].outgoing, mirrors[0].incoming), (1, 3));
    }

    #[test]
    fn two_rows_of_one_account_are_never_one_movement() {
        let main = AccountId::new_random();
        assert!(
            mirrored(&[
                side(1, main, Movement::Out, 250_000),
                side(2, main, Movement::In, 250_000),
            ])
            .pairs
            .is_empty()
        );
    }

    #[test]
    fn a_different_day_is_a_different_movement_within_one_document() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mut into = side(2, savings, Movement::In, 250_000);
        into.date = date!(2025 - 04 - 11);
        assert!(
            mirrored(&[side(1, main, Movement::Out, 250_000), into])
                .pairs
                .is_empty()
        );
    }

    #[test]
    fn two_currencies_of_one_number_are_two_movements() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mut into = side(2, savings, Movement::In, 250_000);
        into.currency = CurrencyCode::Usd;
        assert!(
            mirrored(&[side(1, main, Movement::Out, 250_000), into])
                .pairs
                .is_empty()
        );
    }

    #[test]
    fn two_departures_are_not_a_pair_however_alike_they_are() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        assert!(
            mirrored(&[
                side(1, main, Movement::Out, 250_000),
                side(2, savings, Movement::Out, 250_000),
            ])
            .pairs
            .is_empty()
        );
    }

    #[test]
    fn one_named_side_is_weaker_evidence_than_two_and_says_so() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let mut out = side(1, main, Movement::Out, 250_000);
        out.far_side = Some(savings);
        let mirrors = mirrored(&[out, side(2, savings, Movement::In, 250_000)]).pairs;
        assert_eq!(mirrors[0].evidence, MirrorEvidence::OneSideNamed);
        assert!(mirrors[0].evidence.is_named());
    }

    #[test]
    fn a_departure_with_no_arrival_is_reported_rather_than_left_out() {
        // Answered as an ordinary row it records a movement whose other half
        // does not exist, or files an internal move as spending. Absence from
        // the pairs said neither.
        let main = AccountId::new_random();
        let read = mirrored(&[side(1, main, Movement::Out, 250_000)]);
        assert!(read.pairs.is_empty());
        assert_eq!(
            read.unpaired,
            vec![UnpairedSide {
                row: 1,
                reason: Unpaired::NoCounterpart,
            }],
        );
    }

    #[test]
    fn a_departure_that_could_have_gone_to_two_accounts_is_ambiguous_and_not_uncounterparted() {
        // The refusal this module already makes, said in its own word. This
        // document holds two rows that could be its other half, which is the
        // opposite of holding none, and one sentence for the two would tell the
        // owner the wrong thing about both.
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let reserve = AccountId::new_random();
        let read = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, savings, Movement::In, 250_000),
            side(3, reserve, Movement::In, 250_000),
        ]);
        assert!(read.pairs.is_empty());
        assert_eq!(
            read.unpaired,
            vec![
                UnpairedSide {
                    row: 1,
                    reason: Unpaired::Ambiguous,
                },
                UnpairedSide {
                    row: 2,
                    reason: Unpaired::Ambiguous,
                },
                UnpairedSide {
                    row: 3,
                    reason: Unpaired::Ambiguous,
                },
            ],
        );
    }

    #[test]
    fn the_leftover_of_two_identical_departures_has_no_counterpart() {
        // One arrival cannot be the other half of two departures, so the
        // departure left over is a departure this document holds no counterpart
        // for — not a departure the document could not choose a partner for.
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let read = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, main, Movement::Out, 250_000),
            side(3, savings, Movement::In, 250_000),
        ]);
        assert_eq!(read.pairs.len(), 1);
        assert_eq!(
            read.unpaired,
            vec![UnpairedSide {
                row: 2,
                reason: Unpaired::NoCounterpart,
            }],
        );
    }

    #[test]
    fn a_paired_side_is_not_reported_as_unpaired() {
        let main = AccountId::new_random();
        let savings = AccountId::new_random();
        let read = mirrored(&[
            side(1, main, Movement::Out, 250_000),
            side(2, savings, Movement::In, 250_000),
        ]);
        assert_eq!(read.pairs.len(), 1);
        assert!(read.unpaired.is_empty());
    }

    #[test]
    fn a_pair_names_the_other_row_from_either_side() {
        let mirror = Mirror {
            outgoing: 4,
            incoming: 9,
            evidence: MirrorEvidence::ShapeAlone,
        };
        assert_eq!(mirror.partner_of(4), Some(9));
        assert_eq!(mirror.partner_of(9), Some(4));
        assert_eq!(mirror.partner_of(5), None);
    }
}
