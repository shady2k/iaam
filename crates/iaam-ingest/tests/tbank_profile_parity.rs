//! The bundled T-Bank profile against the converter it was ported from.
//!
//! **Parity is the acceptance criterion of the port, not a nicety.**
//! `tools/tbank-csv-import/` is the only written statement of this export's
//! rules, and it stays in the tree while the port is proven: it is the oracle,
//! and removing it is a separate act. So this test reads *its* fixtures — not a
//! copy of them, because a second copy is the drift the whole arrangement
//! exists to prevent — and measures the engine's reading against
//! `expected-summary.json`, which was derived from the rules rather than from
//! the code.
//!
//! # What parity can mean here, and what it cannot
//!
//! The converter produces **operations**: `withdrawal`, `deposit`, `transfer`,
//! `income`, `refund`. The engine produces **observations**: the row as the
//! bank stated it, with no operation kind to put a conclusion in. So the two
//! outputs are not the same shape and could not be compared field for field.
//! What is comparable, and what this file asserts, is the money:
//!
//! - the same statement lines are read, and the same ones are not;
//! - the same sums, to the minor unit, with the same directions;
//! - and every judgement the converter made is still **available** to be made
//!   afterwards, because the evidence it was made from is on the observation.
//!
//! Three differences are real, explainable, and deliberate. Each is asserted
//! here so that it stays deliberate.
//!
//! **The two legs of an internal transfer are two rows, not one.** The
//! converter pairs them by equal magnitude within five seconds and drops the
//! second. That window is a *conclusion* — a guess that two rows are one
//! movement — and a profile may not conclude. So the engine holds both legs,
//! and the session's transfer pairing is where a movement gets proposed out of
//! them, visibly and reversibly. The count follows exactly: the engine's rows
//! are the converter's operations **plus** its `dropped_second_leg`.
//!
//! **A row on an account outside the contour is refused by name, not skipped.**
//! The converter counts it in `skipped_outside_contour` and says so on stderr;
//! the engine rejects the row naming the column, the printed string and what
//! would have identified an account. The neighbouring rows are read either way.
//! The difference matters because a silently skipped row is a month of one
//! account missing from a journal that looks complete.
//!
//! **The row identity is over the document and the line, not over the row's
//! text.** The converter keys on `sha256(raw line)` with an ordinal to separate
//! two identical purchases. That is a content digest, which ADR 0017 forbids
//! for the reason the ordinal is patching over: two genuine identical payments
//! are two facts, and a key over their contents merges them. The engine has the
//! bytes, so it uses the pair decision 0017 called sound and could not reach —
//! the document's digest and the row's own line.

use iaam_core::ids::AccountId;
use iaam_core::money::CurrencyCode;
use iaam_ingest::classification::FarSide;
use iaam_ingest::csv_source::{AccountEntry, AccountNames};
use iaam_ingest::observation::{ObservedCounterparty, ObservedDirection, ObservedRow};
use iaam_ingest::profile::{ProfileCatalogue, ReadContext, ReadOutcome, engine};
use time::macros::{date, time};

/// The converter's own fixture, read rather than copied.
const EXPORT: &[u8] =
    include_bytes!("../../../tools/tbank-csv-import/fixtures/synthetic-export.csv");

/// The expectation the converter is checked against, derived from the rules.
const EXPECTED: &str =
    include_str!("../../../tools/tbank-csv-import/fixtures/expected-summary.json");

/// The accounts the converter's `account-map.json` names, invented end to end.
///
/// The map is export name to iaam **title**, so the directory here is titled
/// accounts and the engine resolves through decision 0004's tiering. `Outside`
/// is deliberately absent: it is the fixture's account outside the contour.
struct Directory {
    names: AccountNames,
    main: AccountId,
    savings: AccountId,
}

fn directory() -> Directory {
    let main = AccountId::new_random();
    let savings = AccountId::new_random();
    let elsewhere = AccountId::new_random();
    Directory {
        names: [
            AccountEntry::titled("Main", main),
            AccountEntry::titled("Savings", savings),
            // Named by the converter's account map and never printed in the
            // export's account column. It is the account its counterparty map
            // points at, and the engine still does not reach it — see
            // `the_counterparty_map_question_is_left_open`.
            AccountEntry::titled("Elsewhere", elsewhere),
        ]
        .into_iter()
        .collect(),
        main,
        savings,
    }
}

fn read(directory: &Directory) -> Vec<ReadOutcome> {
    let catalogue = ProfileCatalogue::bundled();
    let installed = catalogue
        .get("tbank-operations-csv")
        .expect("the T-Bank operations export ships");
    engine::read(
        EXPORT,
        &installed.profile,
        &ReadContext {
            accounts: &directory.names,
            // The export prints its own account column, so nothing is declared.
            declared: None,
        },
    )
    .expect("the fixture is a document this profile reads")
    .rows
}

fn observations(rows: &[ReadOutcome]) -> Vec<(u64, &ObservedRow)> {
    rows.iter()
        .filter_map(|outcome| match outcome {
            ReadOutcome::Observed { locator, row } => Some((*locator, row.as_ref())),
            ReadOutcome::Rejected { .. } => None,
        })
        .collect()
}

/// The bundled profile is the one that recognises this export, and no other.
#[test]
fn the_export_is_recognised_by_exactly_one_installed_profile() {
    let catalogue = ProfileCatalogue::bundled();
    let installed = catalogue
        .recognise(EXPORT)
        .expect("one profile recognises the export");
    assert_eq!(installed.profile.id(), "tbank-operations-csv");
    assert_eq!(installed.profile.issuer(), "T-Bank");
    assert_eq!(
        installed.profile.parser_version().0,
        "profile/tbank-operations-csv/1"
    );
}

/// Every statement line is read or refused, and none is silently dropped.
///
/// `rows_in_file` is the converter's own count and it is the first thing parity
/// means: a reader that quietly lost a line would still add up.
#[test]
fn every_line_of_the_export_is_accounted_for() {
    let directory = directory();
    let rows = read(&directory);
    let expected: serde_json::Value = serde_json::from_str(EXPECTED).expect("the expectation");
    let in_file = expected["rows_in_file"].as_u64().expect("rows_in_file");
    assert_eq!(u64::try_from(rows.len()).expect("a small count"), in_file);
    // Locators are lines in the file, header included, so line 1 is the header
    // and the data begins at line 2.
    let locators: Vec<u64> = rows.iter().map(ReadOutcome::locator).collect();
    assert_eq!(locators, (2..=in_file + 1).collect::<Vec<u64>>());
}

/// A row on an account the owner does not have is refused by name, and its
/// neighbours are read.
///
/// The converter counts these in `skipped_outside_contour`; the engine refuses
/// them. The count is the parity, and the visibility is the improvement.
#[test]
fn rows_outside_the_contour_are_refused_by_name_and_counted_the_same() {
    let directory = directory();
    let rows = read(&directory);
    let expected: serde_json::Value = serde_json::from_str(EXPECTED).expect("the expectation");
    let outside = expected["skipped_outside_contour"]
        .as_u64()
        .expect("skipped_outside_contour");
    let refused: Vec<(u64, &iaam_ingest::Rejection)> = rows
        .iter()
        .filter_map(|outcome| match outcome {
            ReadOutcome::Rejected { locator, rejection } => Some((*locator, rejection)),
            ReadOutcome::Observed { .. } => None,
        })
        .collect();
    assert_eq!(
        u64::try_from(refused.len()).expect("a small count"),
        outside
    );
    for (_, rejection) in &refused {
        assert_eq!(rejection.field, "account");
        assert!(
            rejection.actual.contains("Имя счёта"),
            "the refusal names the column the operator has to look at: {rejection:?}"
        );
    }
}

/// The same money, to the minor unit, and in the same directions.
///
/// The one difference is stated as arithmetic rather than described: the
/// engine's rows are the converter's operations **plus** the second leg it
/// dropped, because pairing two legs into one movement is a conclusion and this
/// engine draws none.
#[test]
fn the_engine_reads_the_same_sums_the_converter_submitted_plus_the_leg_it_dropped() {
    let directory = directory();
    let rows = read(&directory);
    let expected: serde_json::Value = serde_json::from_str(EXPECTED).expect("the expectation");

    let mut wanted: Vec<i64> = expected["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .map(|operation| {
            let amount = operation["amount"].as_str().expect("an amount");
            assert_eq!(
                amount.split('.').nth(1).map(str::len),
                Some(2),
                "every sum in this fixture is a two-place rouble amount"
            );
            amount.replace('.', "").parse::<i64>().expect("minor units")
        })
        .collect();
    // The leg the converter dropped: `dropped_second_leg` is one, and the pair
    // it belongs to is the only 5 000,00 transfer in the fixture.
    assert_eq!(
        expected["dropped_second_leg"].as_u64(),
        Some(1),
        "this assertion is written for a fixture with one paired transfer"
    );
    wanted.push(500_000);
    wanted.sort_unstable();

    let observed = observations(&rows);
    let mut found: Vec<i64> = observed
        .iter()
        .map(|(_, row)| row.amount_minor.abs())
        .collect();
    found.sort_unstable();
    assert_eq!(found, wanted);

    let (out, into): (Vec<&(u64, &ObservedRow)>, Vec<&(u64, &ObservedRow)>) = observed
        .iter()
        .partition(|(_, row)| row.direction == ObservedDirection::Out);
    assert_eq!(out.len(), 6);
    assert_eq!(into.len(), 5);
    for (_, row) in &out {
        assert!(row.amount_minor < 0, "an outflow keeps the source's minus");
    }
    for (_, row) in &into {
        assert!(row.amount_minor > 0);
    }
}

/// Every cell the profile names, on one row, exactly as the export printed it.
///
/// One row spelled out rather than every row asserted loosely: this is where a
/// column read out of the wrong place would show up, and the sums above would
/// not catch a description and a counterparty swapped.
#[test]
fn a_row_is_transcribed_cell_by_cell() {
    let directory = directory();
    let rows = read(&directory);
    let observed = observations(&rows);
    let (locator, row) = observed[0];
    assert_eq!(locator, 2);
    assert_eq!(row.account, directory.main);
    assert_eq!(row.amount_minor, -10_000);
    assert_eq!(row.currency, CurrencyCode::Rub);
    assert_eq!(row.direction, ObservedDirection::Out);
    assert_eq!(row.dates.trade, Some(date!(2026 - 08 - 05)));
    assert_eq!(row.dates.cash_posted, Some(date!(2026 - 08 - 05)));
    assert_eq!(row.source_time, Some(time!(10:00:00)));
    assert_eq!(
        row.counterparty,
        ObservedCounterparty::Named("Shop One".to_owned())
    );
    assert_eq!(row.description.as_deref(), Some("Shop One"));
    // The source's own category, in the field a source's category belongs in —
    // and never mapped to one of the owner's. His category rules do that, they
    // are his, and they can be re-run over rows already recorded.
    assert_eq!(row.source_category.as_deref(), Some("Супермаркеты"));
    // The export prints no operation-type word at all, so there is nothing to
    // transcribe here and the profile names no column for it. A rule written on
    // an operation word will not match a row of this export, and that is the
    // export's silence rather than the profile's.
    assert_eq!(row.source_kind, None);
    // No column of this export asserts in words that the far side is one of the
    // owner's accounts — the phrase that means it is free text in the
    // description — so every row says «unstated», which is what a source that
    // does not make the claim said.
    assert_eq!(row.far_side, FarSide::Unstated);
}

/// Two legs of one internal transfer sit in the reading as two rows, each on
/// its own account, and neither concludes anything about the other.
///
/// This is the shape the session needs: both legs of one transfer have to be
/// able to sit in it before either is recorded.
///
/// # Why this profile states no far side, and the column it refuses to use
///
/// A `far_side` block would let the profile say, in the source's own words,
/// that the other side is an account of the owner's. This export gives it no
/// honest place to say that. The phrase the bank prints for an internal
/// transfer is free text in a description column, and a token map over free
/// text is not total — every merchant's name would reject its row.
///
/// The export does carry a column that separates the internal transfers from
/// everything else in this fixture: the analytics flag, which the bank prints
/// as «no» on exactly those rows. Mapping that to `own_account` would pass this
/// test and be wrong. It is a two-value vocabulary about whether the bank counts
/// a row in **its own** spending report — a setting the owner can also change —
/// and not a statement about whose account is on the far side. Being wrong there
/// is expensive in one direction only: a row wrongly carrying `own_account` is
/// recorded as a movement between the owner's own accounts and **raises no
/// question**, so it is the one mapping that can silence a question that should
/// have been asked. A profile author looking at this export will find that
/// column; this is the note that says why it is not in the profile.
#[test]
fn both_legs_of_one_transfer_are_read_and_neither_is_dropped() {
    let directory = directory();
    let rows = read(&directory);
    let observed = observations(&rows);
    let legs: Vec<&(u64, &ObservedRow)> = observed
        .iter()
        .filter(|(_, row)| row.amount_minor.abs() == 500_000)
        .collect();
    assert_eq!(legs.len(), 2);
    let (out_locator, outgoing) = legs[0];
    let (in_locator, incoming) = legs[1];
    assert_eq!((*out_locator, *in_locator), (5, 6));
    assert_eq!(outgoing.account, directory.main);
    assert_eq!(outgoing.direction, ObservedDirection::Out);
    assert_eq!(incoming.account, directory.savings);
    assert_eq!(incoming.direction, ObservedDirection::In);
    // Both carry the bank's own phrase, which is what pairing and the owner's
    // rules read. Neither carries a claim that the far side is his: the phrase
    // is free text in a description column, and a profile may set
    // `own_account` only from a word in a closed map over a column that states
    // it — see the report accompanying this change.
    for (_, leg) in &legs {
        assert_eq!(
            leg.counterparty,
            ObservedCounterparty::Named("Между своими счетами".to_owned())
        );
        assert_eq!(leg.far_side, FarSide::Unstated);
    }
}

/// **The account-map question is left open, not answered.**
///
/// Which printed name is one of the owner's accounts is his directory's answer
/// and it is given here — the engine resolves the account column through
/// decision 0004's tiering, so the row lands on the right statement. What the
/// engine never does is *invent* the resolution: a name the directory does not
/// know refuses its row rather than being mapped by a file passed on the
/// command line.
#[test]
fn the_account_map_question_is_answered_by_the_directory_and_by_nothing_else() {
    let directory = directory();
    let rows = read(&directory);
    for (_, row) in observations(&rows) {
        assert!(
            row.account == directory.main || row.account == directory.savings,
            "every read row is on an account the directory identified"
        );
    }
    // And with an empty directory, every row of the export refuses — none is
    // guessed onto an account, and the document is still read line by line.
    let empty = Directory {
        names: AccountNames::default(),
        main: directory.main,
        savings: directory.savings,
    };
    let rows = read(&empty);
    assert!(observations(&rows).is_empty());
    assert_eq!(rows.len(), 13);
}

/// **The counterparty-map question is left open**, and the evidence to answer
/// it is on the row.
///
/// Nothing in an export distinguishes a payment to a stranger from a top-up of
/// the same person's account at another bank; both are a name and an amount. The
/// converter takes the answer per run in `--counterparty-map`. The engine
/// transcribes the printed name and stops, so the row reaches the session with
/// the string the owner's directory resolves and his classification rules match
/// — an answer he gives once and keeps, instead of a file he passes every time.
#[test]
fn the_counterparty_map_question_is_left_open() {
    let directory = directory();
    let rows = read(&directory);
    let observed = observations(&rows);
    let (locator, row) = observed
        .iter()
        .find(|(_, row)| {
            row.counterparty == ObservedCounterparty::Named("Elsewhere Self".to_owned())
        })
        .expect("the counterparty the converter's map is about");
    assert_eq!(*locator, 13);
    // Everything the converter concluded from is here, and no conclusion is.
    assert_eq!(row.amount_minor, -200_000);
    assert_eq!(row.direction, ObservedDirection::Out);
    assert_eq!(row.source_category.as_deref(), Some("Переводы"));
    assert_eq!(row.far_side, FarSide::Unstated);
}

/// **The refund question is left open too**, with the evidence beside it.
///
/// The converter's rule — a positive row whose category is a spending category
/// is a merchant giving money back — is the owner's judgement about his bank's
/// categories, not a fact the export states. The engine records the sign and the
/// bank's own category, and the session asks.
#[test]
fn a_positive_row_carrying_a_spending_category_is_read_and_not_concluded() {
    let directory = directory();
    let rows = read(&directory);
    let observed = observations(&rows);
    let (locator, row) = observed
        .iter()
        .find(|(locator, _)| *locator == 14)
        .expect("the returning row");
    assert_eq!(*locator, 14);
    assert_eq!(row.amount_minor, 8_000);
    assert_eq!(row.direction, ObservedDirection::In);
    assert_eq!(row.source_category.as_deref(), Some("Супермаркеты"));

    // And the two rows the converter files as income carry the bank's own words
    // for them, unmapped: what they turn out to be is the owner's rule or his
    // answer, and a mapping baked into a profile would be frozen into every fact
    // at the moment of import.
    let categories: Vec<&str> = observed
        .iter()
        .filter(|(_, row)| row.direction == ObservedDirection::In)
        .filter_map(|(_, row)| row.source_category.as_deref())
        .collect();
    assert!(categories.contains(&"Проценты"), "{categories:?}");
    assert!(categories.contains(&"Бонусы"), "{categories:?}");
}

/// Two identical purchases on one day are two facts and keep two identities.
///
/// The converter needs an ordinal inside its key to say so, because its key is
/// over the row's text. The engine's key is over the document and the line, so
/// the two rows differ without anything being appended to make them.
#[test]
fn two_identical_rows_are_two_identities_without_an_ordinal() {
    let directory = directory();
    let rows = read(&directory);
    let observed = observations(&rows);
    let identical: Vec<&(u64, &ObservedRow)> = observed
        .iter()
        .filter(|(_, row)| {
            row.amount_minor == -10_000
                && row.counterparty == ObservedCounterparty::Named("Shop One".to_owned())
        })
        .collect();
    assert_eq!(identical.len(), 2);
    let keys: Vec<&str> = identical
        .iter()
        .map(|(_, row)| {
            row.identity
                .idempotency_key
                .as_deref()
                .expect("a row that names no identity of its own is given one")
        })
        .collect();
    assert_ne!(keys[0], keys[1]);
    assert!(keys[0].ends_with(":row:2"), "{keys:?}");
    assert!(keys[1].ends_with(":row:4"), "{keys:?}");
    // The document's own digest, and nothing about the profile: a corrected
    // profile re-reading this document derives the same keys, so the second
    // import is answered `duplicate` rather than doubling a month.
    for key in &keys {
        assert!(key.starts_with("profile:v1:"), "{keys:?}");
        assert!(
            !key.contains("tbank"),
            "the key names neither the profile nor its version: {keys:?}"
        );
    }
}
