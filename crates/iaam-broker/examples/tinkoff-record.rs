//! Снятие сырых ответов T-Invest API с песочницы.
//!
//! Команда нужна только для подготовки замороженных образцов: она берёт
//! заведённый доступ из базы, но не печатает и не сохраняет расшифрованный
//! токен. После обезличивания образцы используются тестами без сети.
//!
//! ```text
//! IAAM_DATABASE=/path/to/iaam.sqlite \
//! IAAM_BROKER_KEY_FILE=/path/to/broker.key \
//! nix develop -c cargo run -p iaam-broker --example tinkoff-record
//! ```
//!
//! Каталог образцов задаётся `IAAM_TINKOFF_FIXTURES_DIR` и по умолчанию
//! равен `tests/fixtures/api/`. Длина интервала в полных днях задаётся
//! `IAAM_TINKOFF_INTERVAL_DAYS` и по умолчанию равна 30.

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use iaam_broker::credentials::{BrokerScope, Key, SealedToken, open};
use iaam_broker::environment::Environment;
use iaam_http::client::HttpClient;
use iaam_http::{Destination, HttpRequest, RequestBody};
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::documents::BrokerCode;
use serde_json::{Value, json};

const BROKER: &str = "tinkoff";
const FIXTURES_DIR_ENV: &str = "IAAM_TINKOFF_FIXTURES_DIR";
const INTERVAL_DAYS_ENV: &str = "IAAM_TINKOFF_INTERVAL_DAYS";
const DEFAULT_INTERVAL_DAYS: u64 = 30;

const ACCOUNTS_METHOD: &str = "tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts";
const PORTFOLIO_METHOD: &str =
    "tinkoff.public.invest.api.contract.v1.OperationsService/GetPortfolio";
const OPERATIONS_METHOD: &str =
    "tinkoff.public.invest.api.contract.v1.OperationsService/GetOperationsByCursor";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let fixtures_dir = env::var_os(FIXTURES_DIR_ENV)
        .map_or_else(|| "tests/fixtures/api/".into(), std::path::PathBuf::from);
    let interval_days = interval_days()?;
    let (from, to) = interval(interval_days)?;

    let store = SqliteStore::open(&required_path("IAAM_DATABASE")?)?;
    let key = Key::from_file(&required_path("IAAM_BROKER_KEY_FILE")?)?;
    let owner = match store.sole_token_owner()? {
        SoleOwner::Single(owner) => owner,
        SoleOwner::None => {
            return Err(io::Error::other("в базе нет владельца доступа к брокеру").into());
        }
        SoleOwner::Several => {
            return Err(io::Error::other(
                "в базе несколько владельцев: команда отказывается выбирать одного",
            )
            .into());
        }
    };
    let broker =
        BrokerCode::parse(BROKER).ok_or_else(|| io::Error::other("внутренний код брокера пуст"))?;
    let access = store
        .find_broker_access(owner, &broker, Environment::Sandbox.code())?
        .ok_or_else(|| io::Error::other("песочный доступ к tinkoff не заведён"))?;
    if BrokerScope::parse(&access.scope) != Some(BrokerScope::ReadOnly) {
        return Err(io::Error::other("доступ к tinkoff не имеет только права чтения").into());
    }

    let (nonce, ciphertext) = access.sealed_parts();
    let token = open(&key, &SealedToken::of(nonce.to_vec(), ciphertext.to_vec()))?;
    let client = HttpClient::new();

    // Запрашиваем только действующие счета: закрытый счёт не подходит для
    // следующих вызовов и сделал бы запись образцов случайной.
    let accounts = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        ACCOUNTS_METHOD,
        token.expose(),
        json!({"status": "ACCOUNT_STATUS_OPEN"}),
    )
    .await?;
    write_fixture(&fixtures_dir, "tinkoff-accounts.json", &accounts)?;
    let account_id = account_id(&accounts)?;

    let portfolio = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        PORTFOLIO_METHOD,
        token.expose(),
        json!({"accountId": account_id.as_str()}),
    )
    .await?;
    write_fixture(&fixtures_dir, "tinkoff-portfolio.json", &portfolio)?;

    let operations = fetch_raw(
        &client,
        Destination::TinkoffSandbox,
        OPERATIONS_METHOD,
        token.expose(),
        json!({
            "accountId": account_id.as_str(),
            "from": from,
            "to": to,
            "limit": 1000,
        }),
    )
    .await?;
    // Нельзя заморозить только первую страницу: такой файл выглядел бы полным,
    // но последующие операции остались бы за пределами интервала.
    ensure_complete_operations(&operations)?;
    write_fixture(&fixtures_dir, "tinkoff-operations.json", &operations)?;

    Ok(())
}

fn required_path(variable: &str) -> Result<std::path::PathBuf, io::Error> {
    env::var_os(variable)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("переменная {variable} не задана")))
}

fn interval_days() -> Result<u64, io::Error> {
    let Some(value) = env::var_os(INTERVAL_DAYS_ENV) else {
        return Ok(DEFAULT_INTERVAL_DAYS);
    };
    let value = value
        .into_string()
        .map_err(|_| io::Error::other(format!("{INTERVAL_DAYS_ENV} не является UTF-8")))?;
    let days = value.parse::<u64>().map_err(|_| {
        io::Error::other(format!("{INTERVAL_DAYS_ENV} должно быть целым числом дней"))
    })?;
    if days == 0 {
        return Err(io::Error::other(format!(
            "{INTERVAL_DAYS_ENV} должно быть больше нуля"
        )));
    }
    Ok(days)
}

fn interval(days: u64) -> Result<(String, String), io::Error> {
    let to = OffsetDateTime::now_utc();
    let days = i64::try_from(days)
        .map_err(|_| io::Error::other("интервал слишком велик для календаря"))?;
    let from = to
        .checked_sub(Duration::days(days))
        .ok_or_else(|| io::Error::other("интервал выходит за начало календаря"))?;

    let from = from
        .format(&Rfc3339)
        .map_err(|error| io::Error::other(format!("начало интервала не форматируется: {error}")))?;
    let to = to
        .format(&Rfc3339)
        .map_err(|error| io::Error::other(format!("конец интервала не форматируется: {error}")))?;
    Ok((from, to))
}

async fn fetch_raw(
    client: &HttpClient,
    destination: Destination,
    method: &str,
    token: &str,
    body: Value,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let request = HttpRequest::post(
        destination,
        method,
        RequestBody::Json(serde_json::to_string(&body)?),
    )
    .with_bearer(token);
    let response = client.send(&request).await?;
    if !(200..=299).contains(&response.status) {
        // Тело отказа не попадает ни в файл, ни в ошибку: шлюз не обязан
        // отделять диагностические данные от данных владельца.
        return Err(io::Error::other(format!("{method} вернул HTTP {}", response.status)).into());
    }
    Ok(response.body)
}

fn account_id(body: &[u8]) -> Result<String, Box<dyn Error>> {
    let response: Value = serde_json::from_slice(body)?;
    let accounts = response
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("в ответе GetAccounts нет массива счетов"))?;
    if accounts.is_empty() {
        return Err(io::Error::other("GetAccounts вернул пустой список счетов").into());
    }
    if accounts.len() > 1 {
        return Err(io::Error::other(
            "GetAccounts вернул несколько счетов: команда отказывается выбирать один",
        )
        .into());
    }
    accounts
        .first()
        .and_then(|account| account.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("в ответе GetAccounts нет идентификатора счёта").into())
}

fn ensure_complete_operations(body: &[u8]) -> Result<(), Box<dyn Error>> {
    let response: Value = serde_json::from_slice(body)?;
    let has_next = response
        .get("hasNext")
        .or_else(|| response.get("has_next"))
        .and_then(Value::as_bool)
        .ok_or_else(|| io::Error::other("в ответе операций нет признака пагинации"))?;
    if has_next {
        return Err(io::Error::other(
            "GetOperationsByCursor вернул неполную страницу: образец не записан",
        )
        .into());
    }
    Ok(())
}

fn write_fixture(dir: &Path, name: &str, body: &[u8]) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    let path = dir.join(name);
    fs::write(&path, body)?;
    eprintln!("снят ответ: {}", path.display());
    Ok(())
}
