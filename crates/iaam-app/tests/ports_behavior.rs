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
