use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Clock, Principal, Scope};
use iaam_app::scenarios::reports::{MoneyFlowQuery, account_balances, money_flow};
use iaam_core::contour::{ContourDefinition, ContourId, ContourVersion};
use iaam_core::ids::{AccountId, InstrumentId, OwnerId, SourceId};
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{OperationDates, OperationKind};
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

async fn contour(
    services: &AppServices,
    owner: OwnerId,
    accounts: &[AccountId],
) -> ContourId {
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

async fn append_operation(
    services: &AppServices,
    owner: OwnerId,
    operation: SubmittedOperation,
) {
    let event = normalize(
        &operation,
        iaam_ingest::operation::NormalizationContext {
            owner,
            source: SourceId::new_random(),
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
        },
    )
    .await
    .unwrap_or_else(|error| panic!("report: {error}"));

    assert_eq!(report.version, ContourVersion(1));
    assert_eq!(report.flow.came_in(CurrencyCode::Rub).unwrap().amount().raw(), 300_000);
    assert_eq!(report.flow.went_out(CurrencyCode::Rub).unwrap().amount().raw(), 120_000);
    assert_eq!(report.flow.residual(CurrencyCode::Rub).unwrap().amount().raw(), 0);
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
        },
    )
    .await;

    let rows = account_balances(
        &services,
        &principal(owner),
        contour,
        None,
        date!(2026 - 08 - 31),
    )
    .await
    .unwrap_or_else(|error| panic!("balances: {error}"));

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
    assert_eq!(card_row.cash[0].amount().raw(), 300_000);
    assert_eq!(card_row.positions.len(), 1);
    assert_eq!(card_row.positions[0].1, iaam_core::money::Quantity(Dec::one()));
}
