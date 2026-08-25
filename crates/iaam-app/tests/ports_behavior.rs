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
    assert!(rendered.contains("<скрыт>"));
    assert!(!rendered.contains(secret));
}

#[tokio::test]
async fn unavailable_rule_store_reports_configuration_error() {
    let error = UnavailableClassificationRuleStore
        .list_rules(OwnerId::new_random())
        .await
        .expect_err("недоступное хранилище обязано отказать");

    assert!(matches!(
        error,
        AppError::NotConfigured {
            what: "правила классификации"
        }
    ));
}

/// Отзыв правила проверяется отдельно от чтения списка.
///
/// Заглушка обязана отказывать КАЖДЫМ методом, а не только теми, что
/// возвращают данные. Метод, отвечающий `Ok(())` без настроенного
/// хранилища, сообщает вызывающему, что правило отозвано, — и правило
/// продолжает действовать. Это хуже отказа: отказ виден, а мнимый успех
/// нет.
#[tokio::test]
async fn unavailable_rule_store_refuses_to_retire_a_rule() {
    let error = UnavailableClassificationRuleStore
        .retire_rule(OwnerId::new_random(), uuid::Uuid::new_v4())
        .await
        .expect_err("мнимый успех отзыва оставил бы правило действующим");

    assert!(matches!(
        error,
        AppError::NotConfigured {
            what: "правила классификации"
        }
    ));
}
