//! Точка сборки (§3.2).
//!
//! Единственное место, знающее одновременно про транспорт и про адаптеры.
//! Заслон архитектуры проверяет, что это остаётся правдой.

mod config;
mod provision;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{
    BrokerChannelFactory, BrokerVault, ClassificationRuleStore, Scope, SoleOwner, SystemClock,
    TokenAdmin,
};
use iaam_broker::credentials::Key;
use iaam_broker::environment::Environment;
use iaam_core::ids::OwnerId;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use zeroize::Zeroizing;

use crate::config::Config;

#[derive(Debug, thiserror::Error)]
enum BrokerKeyError {
    #[error(
        "файл ключа {path} не найден; задайте IAAM_GENERATE_BROKER_KEY=1 \
         или выполните make broker-key"
    )]
    Missing {
        path: String,
        #[source]
        source: iaam_broker::credentials::CryptoError,
    },
    #[error(
        "файл ключа {path} существует, но не читается или имеет неверный формат; \
         не создавайте новый поверх него: это сделает нечитаемыми все заведённые доступы"
    )]
    Existing {
        path: String,
        #[source]
        source: iaam_broker::credentials::CryptoError,
    },
}

fn read_broker_key(path: &std::path::Path) -> Result<Key, BrokerKeyError> {
    let path_text = path.display().to_string();
    let missing = matches!(
        std::fs::metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    Key::from_file(path).map_err(|source| {
        if missing {
            BrokerKeyError::Missing {
                path: path_text,
                source,
            }
        } else {
            BrokerKeyError::Existing {
                path: path_text,
                source,
            }
        }
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum RotationConfigError {
    #[error("для ротации задайте IAAM_BROKER_KEY_OLD_FILE")]
    MissingOld,
    #[error("для ротации задайте IAAM_BROKER_KEY_NEW_FILE")]
    MissingNew,
}

fn rotation_paths(
    old: Option<std::ffi::OsString>,
    new: Option<std::ffi::OsString>,
) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>, RotationConfigError> {
    match (old, new) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(RotationConfigError::MissingOld),
        (Some(_), None) => Err(RotationConfigError::MissingNew),
        (Some(old), Some(new)) => Ok(Some((
            std::path::PathBuf::from(old),
            std::path::PathBuf::from(new),
        ))),
    }
}

fn format_error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str("\nпричина: ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn report_error(error: &dyn std::error::Error) {
    eprintln!("ошибка: {}", format_error_chain(error));
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            report_error(error.as_ref());
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Логирование обязательно: без него отладка приёмки невозможна.
    // Чувствительные поля не логируются никогда — сам токен в лог
    // не попадает, только его хеш (§14).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let mut store = SqliteStore::open(&config.database)?;

    // Разовая выдача токена владельца: без него в систему не войти,
    // а откладывать аутентификацию нельзя (§14).
    if let Ok(label) = std::env::var("IAAM_ISSUE_OWNER_TOKEN") {
        // Адаптер без ключа шифрования: выпуску токена ключ не нужен,
        // а требовать его здесь означало бы, что потерянный токен
        // не восстановить, пока не настроен брокер.
        let admin = SqliteAdapter::new(store);
        let token = issue_owner_token(&admin, &label).await?;
        println!("{token}");
        return Ok(());
    }

    // Перешифровка не меняет ни один файл ключей: новый ключ уже
    // подготовлен владельцем, а старый остаётся доступным для отката.
    // Сначала полностью строится новый набор шифротекстов, затем
    // хранилище заменяет его одной транзакцией.
    if let Some((old_path, new_path)) = rotation_paths(
        std::env::var_os("IAAM_BROKER_KEY_OLD_FILE"),
        std::env::var_os("IAAM_BROKER_KEY_NEW_FILE"),
    )
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?
    {
        let old_key = read_broker_key(&old_path).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("старый ключ не прочитан: {error}"),
            )
        })?;
        let new_key = read_broker_key(&new_path).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("новый ключ не прочитан: {error}"),
            )
        })?;
        let rotated = provision::rotate_broker_access(&mut store, &old_key, &new_key)?;
        println!("перешифровано доступов к брокерам: {rotated}");
        return Ok(());
    }

    // Заведение ключа шифрования брокерских доступов. Ключ создаёт сама
    // программа и наружу не отдаёт: то, чего человек не увидел, он не
    // может ни переслать, ни записать не туда (§14).
    if std::env::var("IAAM_GENERATE_BROKER_KEY").is_ok() {
        let path = broker_key_path(&config)?;
        Key::create_at(&path)?;
        println!("ключ заведён: {}", path.display());
        return Ok(());
    }

    // Приём брокерского токена. Токен читается со стандартного ввода,
    // а не из аргумента командной строки: список процессов виден всей
    // машине, а история командной оболочки переживает сессию.
    if let Ok(broker) = std::env::var("IAAM_ADD_BROKER_ACCESS") {
        let key = read_broker_key(&broker_key_path(&config)?)?;
        // Среда называется явно и умолчания не имеет. Умолчание здесь
        // означало бы песочный токен, молча записанный боевым: шлюз
        // ответит отказом на первом же обращении, а по тексту отказа
        // о среде не догадаться — проверено на живом шлюзе.
        let environment = broker_environment()?;
        let id =
            provision::add_broker_access(&mut store, &key, &broker, environment, &read_token()?)?;
        println!(
            "доступ к брокеру {broker} ({}) заведён: {id}",
            environment.code()
        );
        return Ok(());
    }

    // Ключ шифрования брокерских доступов необязателен: без него сервер
    // поднимается, а маршруты брокера отвечают 503. Заданный, но
    // нечитаемый ключ — другое дело: это опечатка в настройке, и молчаливый
    // старт скрыл бы её до первого заведения доступа.
    let broker_key = config
        .broker_key
        .as_deref()
        .map(read_broker_key)
        .transpose()?;

    // Один и тот же адаптер и как хранилище фактов, и как хранилище
    // брокерских доступов: за обоими одно соединение с базой, и второй
    // экземпляр означал бы второго писателя.
    let adapter = Arc::new(SqliteAdapter::with_broker_key(store, broker_key));
    let broker: Arc<dyn BrokerVault> = adapter.clone();
    let channels: Arc<dyn BrokerChannelFactory> = adapter.clone();
    let rules: Arc<dyn ClassificationRuleStore> = adapter.clone();
    let tokens: Arc<dyn TokenAdmin> = adapter.clone();
    let services = Arc::new(AppServices::with_ports(
        adapter.clone(),
        adapter,
        broker,
        tokens,
        Arc::new(SystemClock),
        channels,
        rules,
    ));
    let limiter = Arc::new(RateLimiter::new(config.rate_limit, config.rate_window));
    let state = ServerState::new(services, limiter);

    // Присвоение экземпляра: пока владельца нет, порождается одноразовый
    // код. Печатается в поток ошибок — стандартный вывод занят ответами
    // команд, и подмешивать в него секрет нельзя. Решение «нужен ли код»
    // принимает `iaam-server`: код — состояние его маршрута; здесь
    // остаётся печать, потому что печатать обязана программа, а не
    // библиотека (§14).
    if let Some(code) = iaam_server::claim::arm(&state).await? {
        eprintln!("владельца в базе нет: экземпляр ещё не присвоен.");
        eprintln!("код присвоения (действует 15 минут, одноразовый): {code}");
        eprintln!("обменяйте его на токен владельца одним запросом:");
        eprintln!("  POST /v1/claim {{\"code\": \"…\", \"label\": \"ноутбук\"}}");
    }

    let (router, _api) = build(state);

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address = %config.listen, "сервер запущен");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Среда брокера из окружения. Отсутствие переменной — отказ.
fn broker_environment() -> Result<Environment, Box<dyn std::error::Error>> {
    let value = std::env::var("IAAM_BROKER_ENVIRONMENT").map_err(|_| {
        "переменная IAAM_BROKER_ENVIRONMENT не задана: prod или sandbox. \
         Токены у сред разные, и выбрать за вас нельзя"
    })?;
    Environment::parse(&value)
        .ok_or_else(|| format!("неизвестная среда брокера {value}: бывают prod и sandbox").into())
}

/// Путь к файлу ключа. Отсутствие переменной — отказ, а не умолчание.
fn broker_key_path(config: &Config) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    config
        .broker_key
        .clone()
        .ok_or_else(|| "переменная IAAM_BROKER_KEY_FILE не задана".into())
}

/// Чтение токена со стандартного ввода.
///
/// Возвращается в зануляемой памяти: открытый токен живёт ровно до
/// шифрования и не остаётся в освобождённой памяти процесса.
fn read_token() -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    // Подсказка идёт в поток ошибок: стандартный вывод занят ответом
    // команды, и подмешивать в него приглашение нельзя.
    eprintln!("вставьте токен брокера и завершите ввод (Ctrl-D):");
    let mut token = Zeroizing::new(String::new());
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
    Ok(token)
}

/// Выдача токена владельца. Сам токен печатается **один раз** и нигде
/// не сохраняется: в базе лежит только его хеш.
///
/// **Это дверь восстановления при потере токена.** Восстановление
/// обязано требовать доказательства сильнее того, что восстанавливается,
/// и таким доказательством здесь служит доступ к консоли машины: тот,
/// кто может запустить эту команду, уже может прочитать файл базы.
/// Поэтому дверь остаётся открытой, но остаётся именно консольной —
/// маршрута, выпускающего токен владельца, в API нет и не будет.
///
/// Владелец берётся из базы, а не заводится заново на каждый вызов.
/// Прежняя версия звала `OwnerId::new_random()` всегда: второй запуск
/// заводил второго владельца, портфель первого выглядел пропавшим —
/// токен-то действующий, а событий за новым владельцем нет, — и
/// `sole_token_owner` начинал возвращать `Several`, отказывая
/// в заведении брокерского доступа.
async fn issue_owner_token(
    admin: &dyn TokenAdmin,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let owner = match admin.sole_owner().await? {
        SoleOwner::Single(owner) => owner,
        // Владельца нет — первый выпуск: экземпляр присваивается
        // консолью, минуя одноразовый код.
        SoleOwner::None => OwnerId::new_random(),
        SoleOwner::Several => {
            return Err(
                "владельцев в базе несколько: выбрать за вас, кому выпустить токен, \
                        нельзя. Это следы поломки в однопользовательской системе — \
                        разберитесь с базой, а не с командой"
                    .into(),
            );
        }
    };
    let issued = admin
        .issue_token(owner, label.to_owned(), Scope::Owner)
        .await?;
    Ok(issued.token)
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("получен сигнал остановки");
}

#[cfg(test)]
mod tests {
    use super::{RotationConfigError, format_error_chain, read_broker_key, rotation_paths};

    #[test]
    fn missing_broker_key_explains_generation_command() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-missing-broker-key-{}",
            std::process::id()
        ));
        let error = match read_broker_key(&path) {
            Ok(_) => panic!("тестовый файл ключа неожиданно существует"),
            Err(error) => error,
        };

        let text = format_error_chain(&error);
        assert!(text.contains("IAAM_GENERATE_BROKER_KEY=1"));
        assert!(text.contains("make broker-key"));
        assert!(text.contains("файл ключа"));
        assert!(!text.contains("KeyFileUnreadable"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn invalid_existing_broker_key_warns_against_replacement() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-invalid-broker-key-{}",
            std::process::id()
        ));
        if let Err(error) = std::fs::write(&path, "not-base64") {
            panic!("не удалось подготовить тестовый файл ключа: {error}");
        }

        let error = match read_broker_key(&path) {
            Ok(_) => panic!("испорченный тестовый ключ неожиданно принят"),
            Err(error) => error,
        };
        let _ = std::fs::remove_file(&path);

        let text = format_error_chain(&error);
        assert!(text.contains("существует"));
        assert!(text.contains("неверный формат"));
        assert!(text.contains("не создавайте новый поверх него"));
        assert!(!text.contains("IAAM_GENERATE_BROKER_KEY=1"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn rotation_requires_both_paths_and_never_echoes_values() {
        assert_eq!(
            rotation_paths(None, Some("new-secret-path".into())),
            Err(RotationConfigError::MissingOld)
        );
        assert_eq!(
            rotation_paths(Some("old-secret-path".into()), None),
            Err(RotationConfigError::MissingNew)
        );
        let old_error = rotation_paths(None, Some("new-secret-path".into())).unwrap_err();
        let new_error = rotation_paths(Some("old-secret-path".into()), None).unwrap_err();
        assert!(!old_error.to_string().contains("new-secret-path"));
        assert!(!new_error.to_string().contains("old-secret-path"));
    }
}
