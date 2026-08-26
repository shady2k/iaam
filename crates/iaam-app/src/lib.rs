//! Сценарии и порты (§3.1, §3.2).
//!
//! Оболочка собирает срез, зовёт ядро и сохраняет результат. Арифметики
//! над деньгами здесь нет и быть не может: любое число в ответе API
//! приходит из `iaam-core`.

pub mod adapters;
/// Типы приёмки, доступные транспорту.
///
/// `iaam-server` не зависит от `iaam-ingest` напрямую — это запрещено
/// заслоном архитектуры (§3.2). Приложение переэкспортирует ровно то,
/// что нужно транспорту для преобразования DTO в доменные типы.
pub use iaam_ingest as ingest;

pub mod error;
pub mod jobs;
pub mod ports;
pub mod scenarios;
#[path = "scenarios/sync.rs"]
pub mod sync;
pub mod tokens;

/// Типы SQLite-адаптера, нужные точке сборки и её интеграционным стендам.
///
/// Сервер не импортирует их напрямую: доступ к данным маршруты получают
/// через сценарии приложения.
pub mod storage {
    pub use iaam_store::SqliteStore;
    pub use iaam_store::market::{Coverage, FxRow, KeyRateRow, PriceRow, RunOutcome, SeriesKey};
    pub use iaam_store::reference::{AccountRecord, AliasRecord, InstrumentRecord};
    pub use iaam_store::tokens::{TokenRecord, TokenScope};
}

use std::sync::Arc;

use iaam_store::market::MarketStore;
use ports::{
    BrokerChannelFactory, BrokerVault, ClassificationRuleStore, Clock, InstrumentDirectory,
    MarketData, Store, TokenAdmin, UnavailableBrokerChannelFactory,
    UnavailableClassificationRuleStore, UnavailableMarketData,
};

/// Собранные зависимости. Точка сборки создаёт один экземпляр,
/// обработчики получают `Arc<AppServices>` (§3.2).
pub struct AppServices {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn InstrumentDirectory>,
    /// Хранилище брокерских доступов. Отдельным полем, а не частью
    /// `store`: за ним стоит ключ шифрования, и его может не быть.
    pub broker: Arc<dyn BrokerVault>,
    /// Управление токенами. Отдельным полем по той же причине, по
    /// которой это отдельный порт: читать журнал и раздавать права
    /// на него — разные полномочия (§14).
    pub tokens: Arc<dyn TokenAdmin>,
    pub clock: Arc<dyn Clock>,
    /// Создание канала брокера. Секреты остаются внутри адаптера.
    pub channels: Arc<dyn BrokerChannelFactory>,
    /// Исторические правила классификации.
    pub rules: Arc<dyn ClassificationRuleStore>,
    /// HTTP-порт источников рынка. Без адаптера ручной запуск отвечает 503.
    pub market: Arc<dyn MarketData>,
    /// Отдельное соединение рынка; блокирующие операции не выполняются
    /// в async-обработчике напрямую.
    pub market_store: Arc<tokio::sync::Mutex<MarketStore>>,
}

impl AppServices {
    /// Сборка с умолчаниями для необязательных портов.
    ///
    /// Конструктора со всеми семью портами нет намеренно: поля структуры
    /// публичны, и литерал с именованными полями читается лучше
    /// позиционного вызова, а перестановку двух портов одного вида
    /// компилятор на литерале заметит по имени поля (§15.1).
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        directory: Arc<dyn InstrumentDirectory>,
        broker: Arc<dyn BrokerVault>,
        tokens: Arc<dyn TokenAdmin>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            directory,
            broker,
            tokens,
            clock,
            channels: Arc::new(UnavailableBrokerChannelFactory),
            rules: Arc::new(UnavailableClassificationRuleStore),
            market: Arc::new(UnavailableMarketData),
            market_store: Arc::new(tokio::sync::Mutex::new(
                MarketStore::open_in_memory()
                    .expect("in-memory market store must be constructible"),
            )),
        }
    }
}
