//! Живая проверка канала на песочнице Т-Инвестиций.
//!
//! **Второй режим проверки.** Обычный `cargo test` сети не касается
//! вовсе: он проверяет разбор на замороженных образцах и потому
//! повторяем, быстр и не зависит от чужой доступности. Этот файл
//! собирается только под фичей `sandbox` и отвечает на другой вопрос —
//! не «правильно ли мы разбираем», а «не изменился ли мир под нами»:
//! жив ли шлюз, годен ли вшитый корень доверия, принимает ли брокер
//! заведённый доступ.
//!
//! ```text
//! nix develop -c cargo test -p iaam-broker --features sandbox
//! ```
//!
//! Метод берётся **обычный**, а не из `SandboxService`. Т-Инвестиции
//! предлагают два способа: вызывать обычные методы по адресу песочницы
//! (рекомендуемый) либо пользоваться методами песочницы. Смешение
//! способов — обычный метод по боевому адресу или метод песочницы
//! по адресу песочницы — шлюз встречает `40003`, то есть жалобой
//! на токен, и разбирающийся думает на токен, а не на маршрут.
//!
//! Требует заведённого доступа: `IAAM_DATABASE` и `IAAM_BROKER_KEY_FILE`
//! указывают на базу и ключ, а сам доступ заводится командой
//! `IAAM_ADD_BROKER_ACCESS=tinkoff`. Отсутствие любого из них —
//! **отказ**, а не пропуск: режим запрошен явно, и молча ничего не
//! проверить значит соврать зелёным прогоном.
#![cfg(feature = "sandbox")]

use std::path::PathBuf;

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open};
use iaam_broker::environment::Environment;
use iaam_broker::trust::tinkoff_client;
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::documents::BrokerCode;

fn required(variable: &str) -> PathBuf {
    PathBuf::from(std::env::var(variable).unwrap_or_else(|_| {
        panic!("режим песочницы запрошен, но {variable} не задана: проверять нечем")
    }))
}

#[tokio::test]
async fn the_sandbox_accepts_the_provisioned_access() {
    // Проверяется вся цепочка разом: ключ из файла, шифротекст из базы,
    // расшифровка, вшитый корень доверия и заголовок авторизации.
    // Каждое звено по отдельности проверено обычными тестами; здесь
    // важно, что они сходятся на настоящем шлюзе.
    let store = SqliteStore::open(&required("IAAM_DATABASE")).expect("база открыта");
    let key = Key::from_file(&required("IAAM_BROKER_KEY_FILE")).expect("ключ прочитан");
    let SoleOwner::Single(owner) = store.sole_token_owner().expect("владелец прочитан")
    else {
        panic!("в базе нет единственного владельца: сначала выпустите токен владельца");
    };
    let broker = BrokerCode::parse("tinkoff").expect("код брокера");
    // Среда называется явно: боевым доступом в песочницу не ходят,
    // и «какой найдётся» здесь означало бы поход не туда.
    let access = store
        .find_broker_access(owner, &broker, Environment::Sandbox.code())
        .expect("доступ прочитан")
        .expect("песочный доступ к tinkoff не заведён: POST /v1/broker-access");

    assert_eq!(
        BrokerScope::parse(&access.scope),
        Some(BrokerScope::ReadOnly),
        "доступ заведён не на чтение — к брокеру с ним не ходят"
    );

    let (nonce, ciphertext) = access.sealed_parts();
    let token = open(&key, &SealedToken::of(nonce.to_vec(), ciphertext.to_vec()))
        .expect("доступ расшифрован");

    let response = tinkoff_client()
        .expect("клиент собран")
        .post(format!(
            "{}/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            Environment::Sandbox.base_url()
        ))
        .header("Content-Type", "application/json")
        .bearer_auth(token.expose())
        .body("{}")
        .send()
        .await
        .expect("шлюз ответил");

    // Тело ответа входит в сообщение намеренно: «HTTP 500» без него
    // не отличает сломанный шлюз от негодного токена, а разбираться
    // с этим приходится по одному этому сообщению. Секрета в теле нет:
    // токен туда не возвращается.
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "песочница отклонила заведённый доступ: HTTP {status}: {body}"
    );
}
