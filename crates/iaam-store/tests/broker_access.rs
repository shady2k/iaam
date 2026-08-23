//! Доступ к брокеру в хранилище (§14).

use std::fs;
use std::path::PathBuf;

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, seal};
use iaam_core::ids::OwnerId;
use iaam_store::SqliteStore;
use iaam_store::broker_access::{BrokerAccess, NewBrokerAccess};
use iaam_store::documents::BrokerCode;
use uuid::Uuid;

const SECRET: &str = "t.Xk3nQ7wPz9-secret-broker-token-000";

/// Каталог под файловую базу. Файл нужен буквально: проверяется, что
/// токена нет в байтах на диске, а базу в памяти на диск не посмотришь.
struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("каталог под базу");
        Self { path }
    }

    fn file(&self) -> PathBuf {
        self.path.join("iaam.sqlite3")
    }

    /// Все файлы базы, включая журнал WAL: секрет, не попавший
    /// в основной файл, но осевший в журнале, утёк ровно так же.
    fn bytes(&self) -> Vec<u8> {
        let mut all = Vec::new();
        for entry in fs::read_dir(&self.path).expect("чтение каталога базы") {
            let entry = entry.expect("файл базы");
            all.extend(fs::read(entry.path()).expect("байты файла базы"));
        }
        all
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn access(owner: OwnerId, broker: &str, key: &Key) -> NewBrokerAccess {
    let sealed = seal(key, SECRET);
    NewBrokerAccess {
        id: Uuid::new_v4(),
        owner,
        broker: BrokerCode::parse(broker).unwrap(),
        scope: BrokerScope::ReadOnly.code().to_owned(),
        nonce: sealed.nonce().to_vec(),
        ciphertext: sealed.ciphertext().to_vec(),
    }
}

#[test]
fn a_leaked_database_file_does_not_leak_the_token() {
    // §14 буквально: утечка файла базы не должна давать доступа
    // к брокерскому счёту. Проверяется не «мы позвали шифрование»,
    // а отсутствие подстроки в байтах на диске — единственная форма
    // этой проверки, которую нельзя пройти случайно.
    let directory = TempDatabase::create("leak");
    let key = Key::from_bytes([9; 32]);
    let owner = OwnerId::new_random();
    let entry = access(owner, "tinkoff", &key);
    let ciphertext = entry.ciphertext.clone();
    {
        let mut store = SqliteStore::open(&directory.file()).unwrap();
        store.insert_broker_access(&entry).unwrap();
    }

    let bytes = directory.bytes();
    // Сначала проверяется, что тест вообще смотрит на записанные
    // данные: тест, ищущий секрет в пустом файле, проходит всегда.
    assert!(
        bytes
            .windows(ciphertext.len())
            .any(|window| window == ciphertext.as_slice()),
        "шифротекста нет в файле базы: тест смотрит не туда"
    );
    assert!(
        !bytes
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes()),
        "токен найден в файле базы"
    );
}

#[test]
fn an_archive_bundle_carries_no_broker_access() {
    // Переносимый архив с живым доступом к брокерскому счёту — это
    // способ вынести доступ из системы вместе с архивом.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([8; 32]);
    let owner = OwnerId::new_random();
    let entry = access(owner, "tinkoff", &key);
    let ciphertext = entry.ciphertext.clone();
    store.insert_broker_access(&entry).unwrap();

    let bundle = store.export_bundle(owner).unwrap();
    let serialised = serde_json::to_vec(&bundle).unwrap();

    assert!(
        !serialised
            .windows(ciphertext.len())
            .any(|window| window == ciphertext.as_slice()),
        "шифротекст доступа попал в архив"
    );
}

#[test]
fn a_stored_access_opens_back_with_the_key() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([7; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    store
        .insert_broker_access(&access(owner, "tinkoff", &key))
        .unwrap();

    let found = store.find_broker_access(owner, &broker).unwrap().unwrap();
    assert_eq!(
        BrokerScope::parse(&found.scope),
        Some(BrokerScope::ReadOnly)
    );
    let (nonce, ciphertext) = found.sealed_parts();
    let sealed = SealedToken::of(nonce.to_vec(), ciphertext.to_vec());
    let opened = iaam_broker::credentials::open(&key, &sealed).unwrap();
    assert_eq!(opened.expose(), SECRET);
}

#[test]
fn broker_access_of_another_owner_is_not_found() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([6; 32]);
    let theirs = OwnerId::new_random();
    store
        .insert_broker_access(&access(theirs, "tinkoff", &key))
        .unwrap();

    let stranger = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    assert_eq!(store.find_broker_access(stranger, &broker).unwrap(), None);
    assert_eq!(store.broker_access_history(stranger).unwrap(), vec![]);
}

#[test]
fn a_revoked_access_is_not_found_but_stays_in_history() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([5; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    let entry = access(owner, "tinkoff", &key);
    store.insert_broker_access(&entry).unwrap();

    store.revoke_broker_access(owner, entry.id).unwrap();

    assert_eq!(store.find_broker_access(owner, &broker).unwrap(), None);
    let history = store.broker_access_history(owner).unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].revoked_at.is_some());
}

#[test]
fn a_second_active_access_for_one_broker_is_refused() {
    // Два действующих доступа означают, что неизвестно, каким из них
    // система ходит к брокеру.
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([4; 32]);
    let owner = OwnerId::new_random();
    store
        .insert_broker_access(&access(owner, "tinkoff", &key))
        .unwrap();

    assert!(
        store
            .insert_broker_access(&access(owner, "tinkoff", &key))
            .is_err(),
        "второй действующий доступ к тому же брокеру заведён"
    );
}

#[test]
fn a_revoked_access_makes_room_for_a_new_one() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let key = Key::from_bytes([3; 32]);
    let owner = OwnerId::new_random();
    let broker = BrokerCode::parse("tinkoff").unwrap();
    let first = access(owner, "tinkoff", &key);
    store.insert_broker_access(&first).unwrap();
    store.revoke_broker_access(owner, first.id).unwrap();

    let second = access(owner, "tinkoff", &key);
    store.insert_broker_access(&second).unwrap();

    let found: BrokerAccess = store.find_broker_access(owner, &broker).unwrap().unwrap();
    assert_eq!(found.id, second.id);
}
