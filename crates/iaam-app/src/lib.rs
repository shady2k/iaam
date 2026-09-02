//! Use cases and ports (§3.1, §3.2).
//!
//! The shell assembles a snapshot, calls the core and persists the result. There is
//! no monetary arithmetic here and there never can be: every number in an API response
//! comes from `iaam-core`.

pub mod actions;
pub mod adapters;
pub mod market_candidate;
/// Ingestion types available to the transport layer.
///
/// `iaam-server` does not depend on `iaam-ingest` directly — this is prohibited
/// by the architecture boundary (§3.2). The application re-exports exactly what
/// the transport layer needs to convert DTOs into domain types.
pub use iaam_ingest as ingest;

pub mod error;
pub mod jobs;
pub mod ports;
pub mod scenarios;
#[path = "scenarios/sync.rs"]
pub mod sync;
pub mod tokens;

/// SQLite adapter types needed by the composition root and its integration test harnesses.
///
/// The server does not import them directly: routes access data
/// through application use cases.
pub mod storage {
    pub use iaam_store::SqliteStore;
    pub use iaam_store::documents::BrokerCode;
    pub use iaam_store::market::{Coverage, FxRow, KeyRateRow, PriceRow, RunOutcome, SeriesKey};
    pub use iaam_store::reference::{AccountRecord, AliasRecord, InstrumentRecord};
    pub use iaam_store::tokens::{TokenRecord, TokenScope};
}

use std::sync::Arc;

use iaam_store::market::MarketStore;
use ports::{
    BrokerChannelFactory, BrokerDictionary, BrokerVault, CategoryStore, ClassificationRuleStore,
    Clock, InstrumentDirectory, OutboundHttp, Store, TokenAdmin, UnavailableBrokerChannelFactory,
    UnavailableBrokerDictionary, UnavailableCategoryStore, UnavailableClassificationRuleStore,
    UnavailableOutboundHttp,
};

/// Assembled dependencies. The composition root creates one instance;
/// handlers receive `Arc<AppServices>` (§3.2).
pub struct AppServices {
    pub store: Arc<dyn Store>,
    pub directory: Arc<dyn InstrumentDirectory>,
    /// Broker credential storage. A separate field rather than part of
    /// `store`: it requires an encryption key and may be unavailable.
    pub broker: Arc<dyn BrokerVault>,
    /// Token management. A separate field for the same reason that
    /// this is a separate port: reading the journal and granting access
    /// to it are distinct privileges (§14).
    pub tokens: Arc<dyn TokenAdmin>,
    pub clock: Arc<dyn Clock>,
    /// Broker channel creation. Secrets remain inside the adapter.
    pub channels: Arc<dyn BrokerChannelFactory>,
    /// Owner category reference and assignment rules.
    pub categories: Arc<dyn CategoryStore>,
    /// Historical classification rules.
    pub rules: Arc<dyn ClassificationRuleStore>,
    /// Outbound HTTP. Without an adapter, manual execution returns 503.
    pub http: Arc<dyn OutboundHttp>,
    /// Dictionary of channel operation types.
    pub broker_dictionary: Arc<dyn BrokerDictionary>,
    /// Dedicated market connection; blocking operations are not performed
    /// directly in the async handler.
    pub market_store: Arc<tokio::sync::Mutex<MarketStore>>,
}

impl AppServices {
    /// Construction with defaults for optional ports.
    ///
    /// There is deliberately no constructor taking all seven ports: the struct fields
    /// are public, and a literal with named fields is clearer than
    /// a positional call, while if two ports of the same type are swapped
    /// in a literal, the compiler will identify them by field name (§15.1).
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
            categories: Arc::new(UnavailableCategoryStore),
            clock,
            channels: Arc::new(UnavailableBrokerChannelFactory),
            rules: Arc::new(UnavailableClassificationRuleStore),
            http: Arc::new(UnavailableOutboundHttp),
            broker_dictionary: Arc::new(UnavailableBrokerDictionary),
            market_store: Arc::new(tokio::sync::Mutex::new(
                MarketStore::open_in_memory()
                    .expect("in-memory market store must be constructible"),
            )),
        }
    }
}
