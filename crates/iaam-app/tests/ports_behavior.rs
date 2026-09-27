use iaam_app::error::AppError;
use iaam_app::ports::{
    BrokerEnvironment, ClassificationRuleStore, IssuedToken, Scope,
    UnavailableClassificationRuleStore,
};
use iaam_core::ids::OwnerId;
use uuid::Uuid;

#[test]
fn broker_environment_codes_are_stable_machine_values() {
    assert_eq!(BrokerEnvironment::Prod.code(), "prod");
    assert_eq!(BrokerEnvironment::Sandbox.code(), "sandbox");
}

#[test]
fn issued_token_debug_redacts_the_secret_but_keeps_context() {
    let secret = "tok_live_never_log_this";
    let issued = IssuedToken {
        id: Uuid::new_v4(),
        token: secret.into(),
        label: "automation".into(),
        scope: Scope::Agent,
    };

    let rendered = format!("{issued:?}");
    assert!(rendered.contains("IssuedToken"));
    assert!(rendered.contains("<hidden>"));
    assert!(!rendered.contains(secret));
}

#[tokio::test]
async fn unavailable_rule_store_reports_configuration_error() {
    let error = UnavailableClassificationRuleStore
        .list_rules(OwnerId::new_random())
        .await
        .expect_err("an unavailable store must reject the operation");

    assert!(matches!(
        error,
        AppError::NotConfigured {
            what: "classification rules"
        }
    ));
}

/// Rule revocation is tested separately from reading the list.
///
/// The stub must reject EVERY method, not just those that
/// return data. A method returning `Ok(())` without a configured
/// store tells the caller that the rule has been revoked — yet the rule
/// remains in force. This is worse than failure: failure is visible, but false success
/// is not.
#[tokio::test]
async fn unavailable_rule_store_refuses_to_retire_a_rule() {
    let error = UnavailableClassificationRuleStore
        .retire_rule(OwnerId::new_random(), uuid::Uuid::new_v4())
        .await
        .expect_err("false revocation success would leave the rule in force");

    assert!(matches!(
        error,
        AppError::NotConfigured {
            what: "classification rules"
        }
    ));
}

// --- iaam-xzz5.3.2: the binding port over the real sqlite adapter --------

use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{AccountView, Store};
use iaam_app::storage::{SqliteStore as AppSqliteStore, supported_brokers};
use iaam_core::ids::AccountId;
use iaam_store::documents::BrokerCode;

fn adapter() -> SqliteAdapter {
    SqliteAdapter::new(AppSqliteStore::open_in_memory().expect("in-memory database"))
}

async fn seed_account(adapter: &SqliteAdapter, owner: OwnerId, account: AccountId) {
    adapter
        .upsert_account(
            owner,
            AccountView {
                id: account,
                title: "Main".to_owned(),
                institution: Some("Test Bank".to_owned()),
            },
        )
        .await
        .expect("seed account");
}

#[tokio::test]
async fn a_recorded_binding_reads_back_through_the_port() {
    let adapter = adapter();
    let owner = OwnerId::new_random();
    let account = AccountId::new_random();
    let code = BrokerCode::parse("tinkoff").expect("broker code");
    seed_account(&adapter, owner, account).await;

    adapter
        .record_broker_account_binding(owner, account, &code, "invented-one".to_owned())
        .await
        .expect("the binding is recorded");

    assert_eq!(
        adapter
            .broker_account_binding(owner, account, &code)
            .await
            .expect("the binding is read"),
        Some("invented-one".to_owned()),
        "what the port wrote is what the port reads"
    );
}

#[tokio::test]
async fn the_port_refuses_a_number_another_account_already_holds() {
    let adapter = adapter();
    let owner = OwnerId::new_random();
    let first = AccountId::new_random();
    let second = AccountId::new_random();
    let code = BrokerCode::parse("tinkoff").expect("broker code");
    seed_account(&adapter, owner, first).await;
    seed_account(&adapter, owner, second).await;

    adapter
        .record_broker_account_binding(owner, first, &code, "invented-one".to_owned())
        .await
        .expect("the first binding stands");

    let error = adapter
        .record_broker_account_binding(owner, second, &code, "invented-one".to_owned())
        .await
        .expect_err("one broker number is one iaam account");

    let AppError::Conflict { what } = error else {
        panic!("expected the held-binding conflict, got {error:?}");
    };
    assert!(
        what.contains(&first.inner().to_string()),
        "the conflict names the account that holds the number: {what}"
    );
}

#[tokio::test]
async fn the_supported_broker_vocabulary_is_the_registrys() {
    let codes: Vec<&str> = supported_brokers().collect();
    assert!(
        codes.contains(&"tinkoff") && codes.contains(&"finam"),
        "the vocabulary names every broker the factory opens a channel for: {codes:?}"
    );
}
