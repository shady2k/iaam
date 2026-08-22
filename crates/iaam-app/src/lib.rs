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

use std::sync::Arc;

use ports::{Clock, Store};

/// Собранные зависимости. Точка сборки создаёт один экземпляр,
/// обработчики получают `Arc<AppServices>` (§3.2).
pub struct AppServices {
    pub store: Arc<dyn Store>,
    pub clock: Arc<dyn Clock>,
}

impl AppServices {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }
}
