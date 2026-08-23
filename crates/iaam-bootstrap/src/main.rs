//! Точка сборки (§3.2).
//!
//! Единственное место, знающее одновременно про транспорт и про адаптеры.
//! Заслон архитектуры проверяет, что это остаётся правдой.

mod config;
mod provision;

use std::sync::Arc;

use iaam_app::AppServices;
use iaam_app::adapters::sqlite::SqliteAdapter;
use iaam_app::ports::{BrokerVault, Scope, SoleOwner, SystemClock, TokenAdmin};
use iaam_broker::credentials::Key;
use iaam_core::ids::OwnerId;
use iaam_server::rate_limit::RateLimiter;
use iaam_server::{ServerState, build};
use iaam_store::SqliteStore;
use zeroize::Zeroizing;

use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        let key = Key::from_file(&broker_key_path(&config)?)?;
        let id = provision::add_broker_access(&mut store, &key, &broker, &read_token()?)?;
        println!("доступ к брокеру {broker} заведён: {id}");
        return Ok(());
    }

    // Ключ шифрования брокерских доступов необязателен: без него сервер
    // поднимается, а маршруты брокера отвечают 503. Заданный, но
    // нечитаемый ключ — другое дело: это опечатка в настройке, и молчаливый
    // старт скрыл бы её до первого заведения доступа.
    let broker_key = config.broker_key.as_deref().map(Key::from_file).transpose()?;

    // Один и тот же адаптер и как хранилище фактов, и как хранилище
    // брокерских доступов: за обоими одно соединение с базой, и второй
    // экземпляр означал бы второго писателя.
    let adapter = Arc::new(SqliteAdapter::with_broker_key(store, broker_key));
    let broker: Arc<dyn BrokerVault> = adapter.clone();
    let tokens: Arc<dyn TokenAdmin> = adapter.clone();
    let services = Arc::new(AppServices::new(adapter, broker, tokens, Arc::new(SystemClock)));
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
            return Err("владельцев в базе несколько: выбрать за вас, кому выпустить токен, \
                        нельзя. Это следы поломки в однопользовательской системе — \
                        разберитесь с базой, а не с командой"
                .into());
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
