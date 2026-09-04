use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Clock, Principal, Scope};
use iaam_app::scenarios::reports::{
    AccountStanding, HeldScope, KnownAccountCoverage, MoneyFlowQuery, account_balances, money_flow,
};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::event::provenance::ParserVersion;
use iaam_core::ids::{AccountId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind, PARSER_VERSION};
use iaam_ingest::{SubmittedOperation, normalize};
use iaam_store::SqliteStore;
use time::Date;
use time::macros::date;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn today(&self) -> Date {
        date!(2026 - 08 - 31)
    }
}

fn services() -> AppServices {
    let adapter = Arc::new(SqliteAdapter::new(
        SqliteStore::open_in_memory().unwrap_or_else(|error| panic!("memory store: {error}")),
    ));
    AppServices::new(
        adapter.clone(),
        adapter.clone(),
        adapter.clone(),
        adapter,
        Arc::new(FixedClock),
    )
}

fn principal(owner: OwnerId) -> Principal {
    Principal {
        token_id: Uuid::new_v4(),
        owner,
        scope: Scope::Owner,
    }
}

async fn account(services: &AppServices, owner: OwnerId, title: &str) -> AccountId {
    let id = AccountId::new_random();
    services
        .store
        .upsert_account(
            owner,
            AccountView {
                id,
                title: title.to_owned(),
                institution: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("insert account: {error}"));
    id
}

async fn contour(services: &AppServices, owner: OwnerId, accounts: &[AccountId]) -> ContourId {
    let id = ContourId::new_random();
    let definition = ContourDefinition::new(id, ContourVersion(1), accounts.iter().copied());
    services
        .store
        .insert_contour_version(
            owner,
            definition,
            "test contour".to_owned(),
            accounts.to_vec(),
        )
        .await
        .unwrap_or_else(|error| panic!("insert contour: {error}"));
    id
}

async fn append_operation(services: &AppServices, owner: OwnerId, operation: SubmittedOperation) {
    let event = normalize(
        &operation,
        &iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
            parser_version: ParserVersion(PARSER_VERSION.to_owned()),
        },
    )
    .unwrap_or_else(|error| panic!("normalize operation: {error:?}"))
    .event;
    services
        .store
        .append_events(vec![event], IdentityScope::Source)
        .await
        .unwrap_or_else(|error| panic!("append operation: {error}"));
}

fn cash_operation(account: AccountId, kind: OperationKind, on: Date) -> SubmittedOperation {
    SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(on),
            settled: Some(on),
            cash_posted: Some(on),
            paid: None,
        },
        source_time: None,
        idempotency_key: None,
        source_operation_id: None,
        source_category: None,
        source_kind: None,
        description: None,
    }
}

#[tokio::test]
async fn a_month_of_a_card_reports_what_came_in_and_what_went_out() {
    let services = services();
    let owner = OwnerId::new_random();
    let card = account(&services, owner, "Card").await;
    let contour = contour(&services, owner, &[card]).await;

    append_operation(
        &services,
        owner,
        cash_operation(
            card,
            OperationKind::Deposit {
                amount_minor: 300_000,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 08 - 05),
        ),
    )
    .await;
    append_operation(
        &services,
        owner,
        cash_operation(
            card,
            OperationKind::Withdrawal {
                amount_minor: 120_000,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 08 - 12),
        ),
    )
    .await;
    append_operation(
        &services,
        owner,
        cash_operation(
            card,
            OperationKind::Withdrawal {
                amount_minor: 999_999,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 07 - 30),
        ),
    )
    .await;

    let report = money_flow(
        &services,
        &principal(owner),
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
            held: HeldScope::None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("report: {error}"))
    .report;

    assert_eq!(report.version, ContourVersion(1));
    assert_eq!(
        report
            .flow
            .came_in(CurrencyCode::Rub)
            .unwrap()
            .amount()
            .raw(),
        300_000
    );
    assert_eq!(
        report
            .flow
            .went_out(CurrencyCode::Rub)
            .unwrap()
            .amount()
            .raw(),
        120_000
    );
    assert_eq!(
        report
            .flow
            .residual(CurrencyCode::Rub)
            .unwrap()
            .amount()
            .raw(),
        0
    );
}

#[tokio::test]
async fn a_reversed_interval_is_rejected_by_the_period_field() {
    let services = services();
    let owner = OwnerId::new_random();
    let card = account(&services, owner, "Card").await;
    let contour = contour(&services, owner, &[card]).await;

    let error = money_flow(
        &services,
        &principal(owner),
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 31),
            to: date!(2026 - 08 - 01),
            held: HeldScope::None,
        },
    )
    .await
    .expect_err("reversed interval must be rejected");

    assert!(matches!(
        error,
        iaam_app::error::AppError::Invalid { ref field, .. } if field == "period"
    ));
}

#[tokio::test]
async fn an_account_with_no_movements_still_appears_without_combining_balances() {
    let services = services();
    let owner = OwnerId::new_random();
    let card = account(&services, owner, "Card").await;
    let untouched = account(&services, owner, "Untouched").await;
    let contour = contour(&services, owner, &[card, untouched]).await;
    let instrument = InstrumentId::new_random();

    append_operation(
        &services,
        owner,
        cash_operation(
            card,
            OperationKind::Deposit {
                amount_minor: 300_000,
                currency: CurrencyCode::Rub,
            },
            date!(2026 - 08 - 05),
        ),
    )
    .await;
    append_operation(
        &services,
        owner,
        SubmittedOperation {
            account: card,
            kind: OperationKind::OpeningPosition {
                instrument,
                custody: iaam_core::ids::CustodyId::new_random(),
                quantity: Dec::one(),
                cost_basis_minor: None,
                currency: CurrencyCode::Rub,
                assertions: None,
            },
            dates: OperationDates {
                trade: Some(date!(2026 - 08 - 05)),
                settled: Some(date!(2026 - 08 - 05)),
                cash_posted: Some(date!(2026 - 08 - 05)),
                paid: None,
            },
            source_time: None,
            idempotency_key: None,
            source_operation_id: None,
            source_category: None,
            source_kind: None,
            description: None,
        },
    )
    .await;

    let report = account_balances(
        &services,
        &principal(owner),
        contour,
        None,
        date!(2026 - 08 - 31),
        &HeldScope::None,
    )
    .await
    .unwrap_or_else(|error| panic!("balances: {error}"))
    .report;
    let rows = &report.accounts;

    assert_eq!(rows.len(), 2);
    let untouched_row = rows
        .iter()
        .find(|row| row.account == untouched)
        .expect("untouched account present");
    assert!(untouched_row.cash.is_empty());
    assert!(untouched_row.positions.is_empty());

    let card_row = rows
        .iter()
        .find(|row| row.account == card)
        .expect("card account present");
    assert_eq!(card_row.cash.len(), 1);
    assert_eq!(card_row.cash[0].money.amount().raw(), 300_000);
    // Nothing asserts what the card held before its first movement, so the
    // figure is a running sum rather than a balance.
    assert_eq!(
        card_row.cash[0].opening,
        iaam_app::scenarios::reports::CashOpening::Unasserted
    );
    assert!(report.negative_cash.is_empty());
    assert_eq!(card_row.positions.len(), 1);
    assert_eq!(
        card_row.positions[0].1,
        iaam_core::money::Quantity(Dec::one())
    );
}

/// A report over a contour that leaves out an account nobody has ruled on says
/// so.
///
/// The quality fields of a report all concern defects inside the calculation,
/// and every one of them can be clean while half the owner's money sat outside
/// the contour the figures were folded over. Completeness of a calculation and
/// completeness of its population are two statements, and a reader who is given
/// only the first reads a partial answer as a whole one.
#[tokio::test]
async fn balances_name_the_account_left_outside_that_nobody_has_ruled_on() {
    let services = services();
    let owner = OwnerId::new_random();
    let main = account(&services, owner, "Main").await;
    let savings = account(&services, owner, "Savings").await;
    let contour = contour(&services, owner, &[main]).await;

    let report = account_balances(
        &services,
        &principal(owner),
        contour,
        None,
        date!(2026 - 08 - 31),
        &HeldScope::None,
    )
    .await
    .unwrap_or_else(|error| panic!("balances: {error}"))
    .report;

    let population = &report.population;
    assert_eq!(
        population
            .covered()
            .map(|entry| entry.account)
            .collect::<Vec<_>>(),
        vec![main],
        "the covered set must be the contour the fold used"
    );
    // The rows are the covered set, not a set selected beside it: one
    // population serves the answer and the manifest, so they cannot drift.
    assert_eq!(
        report
            .accounts
            .iter()
            .map(|row| row.account)
            .collect::<Vec<_>>(),
        vec![main]
    );
    let outside: Vec<_> = population.outside().collect();
    assert_eq!(outside.len(), 1, "Savings is known and outside the contour");
    assert_eq!(outside[0].account, savings);
    assert_eq!(outside[0].title, "Savings");
    assert_eq!(outside[0].standing, AccountStanding::OutsideUndecided);
    assert_eq!(
        population.known_account_coverage(),
        KnownAccountCoverage::Undecided,
        "an account in no contour at all is one nobody has ruled on"
    );
}

/// The third distinction: an account placed in another contour is outside on a
/// decision, and one in no contour is outside because nothing decided anything.
#[tokio::test]
async fn an_account_placed_in_another_contour_is_outside_on_a_decision() {
    let services = services();
    let owner = OwnerId::new_random();
    let main = account(&services, owner, "Main").await;
    let savings = account(&services, owner, "Savings").await;
    let reported = contour(&services, owner, &[main]).await;
    // The owner has placed Savings somewhere: it is outside this report, but not
    // outside every decision he has made.
    let _elsewhere = contour(&services, owner, &[savings]).await;

    let report = account_balances(
        &services,
        &principal(owner),
        reported,
        None,
        date!(2026 - 08 - 31),
        &HeldScope::None,
    )
    .await
    .unwrap_or_else(|error| panic!("balances: {error}"))
    .report;

    let outside: Vec<_> = report.population.outside().collect();
    assert_eq!(outside.len(), 1);
    assert_eq!(outside[0].account, savings);
    assert_eq!(outside[0].standing, AccountStanding::OutsidePlacedElsewhere);
    assert_eq!(
        report.population.known_account_coverage(),
        KnownAccountCoverage::Bounded,
        "every account outside this report has been placed in a contour"
    );
}

/// The manifest is the contour the fold used, not a second answer beside it:
/// widening the contour widens the manifest.
#[tokio::test]
async fn changing_the_contour_changes_the_population_the_report_names() {
    let services = services();
    let owner = OwnerId::new_random();
    let main = account(&services, owner, "Main").await;
    let savings = account(&services, owner, "Savings").await;
    let whole = contour(&services, owner, &[main, savings]).await;

    let report = account_balances(
        &services,
        &principal(owner),
        whole,
        None,
        date!(2026 - 08 - 31),
        &HeldScope::None,
    )
    .await
    .unwrap_or_else(|error| panic!("balances: {error}"))
    .report;

    let covered: Vec<_> = report
        .population
        .covered()
        .map(|entry| entry.account)
        .collect();
    assert_eq!(covered.len(), 2);
    assert!(covered.contains(&main) && covered.contains(&savings));
    assert_eq!(report.population.outside().count(), 0);
    assert_eq!(
        report.population.known_account_coverage(),
        KnownAccountCoverage::Whole
    );
    assert_eq!(
        covered.len(),
        report.accounts.len(),
        "the rows and the manifest are the same account set"
    );
}

/// Flow answers about a population too, and states the same one.
#[tokio::test]
async fn the_flow_report_names_the_population_it_covered() {
    let services = services();
    let owner = OwnerId::new_random();
    let main = account(&services, owner, "Main").await;
    let savings = account(&services, owner, "Savings").await;
    let contour = contour(&services, owner, &[main]).await;

    let report = money_flow(
        &services,
        &principal(owner),
        &MoneyFlowQuery {
            contour,
            contour_version: None,
            from: date!(2026 - 08 - 01),
            to: date!(2026 - 08 - 31),
            held: HeldScope::None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("flow: {error}"));

    assert_eq!(
        report
            .population
            .covered()
            .map(|entry| entry.account)
            .collect::<Vec<_>>(),
        vec![main]
    );
    let outside: Vec<_> = report.population.outside().collect();
    assert_eq!(outside.len(), 1);
    assert_eq!(outside[0].account, savings);
    assert_eq!(outside[0].standing, AccountStanding::OutsideUndecided);
    assert_eq!(
        report.population.known_account_coverage(),
        KnownAccountCoverage::Undecided
    );
}
