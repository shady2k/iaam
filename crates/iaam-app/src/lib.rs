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
pub mod ports;
pub mod scenarios;
pub mod tokens;

use std::sync::Arc;

use ports::{BrokerVault, Clock, Store, TokenAdmin};

/// Собранные зависимости. Точка сборки создаёт один экземпляр,
/// обработчики получают `Arc<AppServices>` (§3.2).
pub struct AppServices {
    pub store: Arc<dyn Store>,
    /// Хранилище брокерских доступов. Отдельным полем, а не частью
    /// `store`: за ним стоит ключ шифрования, и его может не быть.
    pub broker: Arc<dyn BrokerVault>,
    /// Управление токенами. Отдельным полем по той же причине, по
    /// которой это отдельный порт: читать журнал и раздавать права
    /// на него — разные полномочия (§14).
    pub tokens: Arc<dyn TokenAdmin>,
    pub clock: Arc<dyn Clock>,
}

impl AppServices {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        broker: Arc<dyn BrokerVault>,
        tokens: Arc<dyn TokenAdmin>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            broker,
            tokens,
            clock,
        }
    }
}
