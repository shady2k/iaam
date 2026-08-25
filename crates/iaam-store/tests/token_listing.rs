use iaam_core::ids::OwnerId;
use iaam_store::SqliteStore;
use iaam_store::tokens::{TokenRecord, TokenScope};
use uuid::Uuid;

#[test]
fn list_tokens_returns_owner_tokens_without_their_secret_hashes() {
    let store = SqliteStore::open_in_memory().unwrap();
    let owner = OwnerId::new_random();
    let record = TokenRecord {
        id: Uuid::new_v4(),
        owner,
        label: "отчёты".into(),
        scope: TokenScope::ReadOnly,
        revoked: false,
    };

    store.insert_token(&record, "секретный-хеш").unwrap();
    let listed = store.list_tokens(owner).unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, record.id);
    assert_eq!(listed[0].label, record.label);
    assert_eq!(listed[0].scope, record.scope);
    assert_eq!(listed[0].revoked_at, None);
}
