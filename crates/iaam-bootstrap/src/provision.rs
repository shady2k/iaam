//! Заведение брокерского доступа (§14).
//!
//! Токен приходит от владельца и **немедленно** шифруется: открытым
//! он существует ровно на время этой функции. Ни в лог, ни в базу,
//! ни в текст ошибки он не попадает — ошибки здесь называют поле
//! и причину, но никогда значение.

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open, seal};
use iaam_broker::environment::Environment;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{BrokerAccessCiphertext, NewBrokerAccess, SoleOwner};
use iaam_store::documents::BrokerCode;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("код брокера пуст")]
    BrokerNotNamed,
    #[error("токен пуст")]
    TokenEmpty,
    #[error("владелец не найден: сначала выпустите токен владельца через IAAM_ISSUE_OWNER_TOKEN")]
    NoOwner,
    #[error("владельцев несколько: выбрать за вас, кому завести доступ, нельзя")]
    SeveralOwners,
    #[error("доступ не сохранён: {0}")]
    NotStored(#[from] iaam_store::StoreError),
    #[error("доступ не удалось открыть старым ключом: {0}")]
    OldKey(#[from] iaam_broker::credentials::CryptoError),
}

/// Завести доступ к брокеру.
///
/// Возвращает идентификатор записи — по нему доступ отзывают. Сам токен
/// не возвращается и не печатается: то, чего вызывающий не получил, он
/// не может выдать наружу.
pub fn add_broker_access(
    store: &mut SqliteStore,
    key: &Key,
    broker: &str,
    environment: Environment,
    token: &str,
) -> Result<Uuid, ProvisionError> {
    let broker = BrokerCode::parse(broker).ok_or(ProvisionError::BrokerNotNamed)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(ProvisionError::TokenEmpty);
    }
    let owner = match store.sole_token_owner()? {
        SoleOwner::Single(owner) => owner,
        SoleOwner::None => return Err(ProvisionError::NoOwner),
        SoleOwner::Several => return Err(ProvisionError::SeveralOwners),
    };

    let sealed = seal(key, token);
    let access = NewBrokerAccess {
        id: Uuid::new_v4(),
        owner,
        broker,
        // Среда называется при заведении: токены у сред разные, и
        // угадать за человека, какой он держит, нельзя.
        environment: environment.code().to_owned(),
        // Область прав задаётся здесь, а не приходит снаружи: торговые
        // права не запрашиваются ни при каких условиях (§14).
        scope: BrokerScope::ReadOnly.code().to_owned(),
        nonce: sealed.nonce().to_vec(),
        ciphertext: sealed.ciphertext().to_vec(),
    };
    store.insert_broker_access(&access)?;
    Ok(access.id)
}

/// Перешифровать всю историю доступов в одной транзакции хранилища.
///
/// Старый ключ используется только до подготовки полного списка новых
/// шифротекстов. Хранилище ничего не меняет, пока расшифровка каждой
/// строки не прошла успешно.
pub fn rotate_broker_access(
    store: &mut SqliteStore,
    old_key: &Key,
    new_key: &Key,
) -> Result<usize, ProvisionError> {
    let history = store.all_broker_access_history()?;
    let mut replacements = Vec::with_capacity(history.len());
    for access in &history {
        let sealed = SealedToken::of(access.nonce.clone(), access.ciphertext.clone());
        let token = open(old_key, &sealed)?;
        let replacement = seal(new_key, token.expose());
        replacements.push(BrokerAccessCiphertext {
            id: access.id,
            nonce: replacement.nonce().to_vec(),
            ciphertext: replacement.ciphertext().to_vec(),
        });
    }
    store.rotate_broker_access_ciphertexts(&replacements)?;
    Ok(replacements.len())
}

#[cfg(test)]
mod tests {
    use iaam_broker::credentials::open;
    use iaam_core::ids::OwnerId;
    use iaam_store::broker_access::BrokerAccess;
    use iaam_store::tokens::{TokenRecord, TokenScope};

    use super::*;

    const TOKEN: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

    fn key() -> Key {
        Key::from_bytes([11; 32])
    }

    fn store_with_owner() -> (SqliteStore, OwnerId) {
        let store = SqliteStore::open_in_memory().unwrap();
        let owner = OwnerId::new_random();
        issue(&store, owner, "ноутбук");
        (store, owner)
    }

    fn issue(store: &SqliteStore, owner: OwnerId, label: &str) {
        store
            .insert_token(
                &TokenRecord {
                    id: Uuid::new_v4(),
                    owner,
                    label: label.to_owned(),
                    scope: TokenScope::Owner,
                    revoked: false,
                },
                &format!("хеш-{label}-{}", owner.inner()),
            )
            .unwrap();
    }

    #[test]
    fn a_provisioned_access_is_stored_sealed_and_read_only() {
        let (mut store, owner) = store_with_owner();
        let key = key();

        let id =
            add_broker_access(&mut store, &key, "tinkoff", Environment::Sandbox, TOKEN).unwrap();

        let broker = BrokerCode::parse("tinkoff").unwrap();
        let found: BrokerAccess = store
            .find_broker_access(owner, &broker, Environment::Sandbox.code())
            .unwrap()
            .unwrap();
        assert_eq!(found.id, id);
        assert_eq!(
            BrokerScope::parse(&found.scope),
            Some(BrokerScope::ReadOnly)
        );
        let (nonce, ciphertext) = found.sealed_parts();
        let sealed = iaam_broker::credentials::SealedToken::of(nonce.to_vec(), ciphertext.to_vec());
        assert_eq!(open(&key, &sealed).unwrap().expose(), TOKEN);
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_the_token() {
        // Токен приходит со стандартного ввода, и перевод строки в конце
        // там неизбежен. Заголовок с лишним пробелом брокер отвергнет,
        // а причину назовёт невнятно.
        let (mut store, owner) = store_with_owner();
        let key = key();

        add_broker_access(
            &mut store,
            &key,
            "tinkoff",
            Environment::Sandbox,
            &format!("  {TOKEN}\n"),
        )
        .unwrap();

        let broker = BrokerCode::parse("tinkoff").unwrap();
        let found = store
            .find_broker_access(owner, &broker, Environment::Sandbox.code())
            .unwrap()
            .unwrap();
        let (nonce, ciphertext) = found.sealed_parts();
        let sealed = iaam_broker::credentials::SealedToken::of(nonce.to_vec(), ciphertext.to_vec());
        assert_eq!(open(&key, &sealed).unwrap().expose(), TOKEN);
    }

    #[test]
    fn an_empty_token_is_refused() {
        let (mut store, _) = store_with_owner();
        assert!(matches!(
            add_broker_access(&mut store, &key(), "tinkoff", Environment::Sandbox, "   \n"),
            Err(ProvisionError::TokenEmpty)
        ));
    }

    #[test]
    fn a_broker_without_a_name_is_refused() {
        let (mut store, _) = store_with_owner();
        assert!(matches!(
            add_broker_access(&mut store, &key(), "  ", Environment::Sandbox, TOKEN),
            Err(ProvisionError::BrokerNotNamed)
        ));
    }

    #[test]
    fn without_an_owner_nothing_is_provisioned() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        assert!(matches!(
            add_broker_access(&mut store, &key(), "tinkoff", Environment::Sandbox, TOKEN),
            Err(ProvisionError::NoOwner)
        ));
    }

    #[test]
    fn between_several_owners_nothing_is_guessed() {
        let (mut store, _) = store_with_owner();
        issue(&store, OwnerId::new_random(), "второй");

        assert!(matches!(
            add_broker_access(&mut store, &key(), "tinkoff", Environment::Sandbox, TOKEN),
            Err(ProvisionError::SeveralOwners)
        ));
    }

    #[test]
    fn no_error_message_carries_the_token() {
        // Сообщение об ошибке — это то, что точно попадёт в лог.
        let mut store = SqliteStore::open_in_memory().unwrap();
        let error = add_broker_access(&mut store, &key(), "tinkoff", Environment::Sandbox, TOKEN)
            .unwrap_err();
        assert!(!error.to_string().contains(TOKEN));
        assert!(!format!("{error:?}").contains(TOKEN));
    }

    #[test]
    fn key_rotation_reencrypts_active_and_revoked_accesses() {
        let (mut store, owner) = store_with_owner();
        let old_key = key();
        let new_key = Key::from_bytes([12; 32]);
        let first = add_broker_access(&mut store, &old_key, "tinkoff", Environment::Sandbox, TOKEN)
            .unwrap();
        store.revoke_broker_access(owner, first).unwrap();
        add_broker_access(&mut store, &old_key, "tinkoff", Environment::Sandbox, TOKEN).unwrap();

        let rotated = rotate_broker_access(&mut store, &old_key, &new_key).unwrap();
        assert_eq!(rotated, 2);
        for entry in store.broker_access_history(owner).unwrap() {
            let sealed = iaam_broker::credentials::SealedToken::of(
                entry.nonce.clone(),
                entry.ciphertext.clone(),
            );
            assert_eq!(open(&new_key, &sealed).unwrap().expose(), TOKEN);
            assert!(open(&old_key, &sealed).is_err());
        }
    }
}
