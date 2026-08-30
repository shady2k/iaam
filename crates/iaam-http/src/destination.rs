//! External endpoints used by the program.
//!
//! This enum is exhaustive and deliberately **not** `#[non_exhaustive]`
//! (§15.1): a new source must break the build here and in the anchor table
//! (`trust.rs`), so its trust policy cannot be forgotten.

/// External endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Destination {
    /// Production T-Invest gateway.
    TinkoffProd,
    /// T-Invest sandbox. A separate destination, not a separate path:
    /// the sandbox has a **different host**, so it cannot be substituted by
    /// trimming the production address's base.
    TinkoffSandbox,
    FinamApi,
    MoexIss,
    /// CBR's simple XML scripts: rates for a date and a period.
    CbrScripts,
    /// CBR's SOAP service: the key rate and other dated series.
    CbrDailyInfo,
    /// Published T-Invest API contract.
    ///
    /// Not the broker gateway, but the contract's source text: operation-kind
    /// codes are checked against it. A separate destination is required
    /// because it has a different host and trust anchor — the gateway's
    /// embedded root does not apply, and using it for another repository
    /// would assert that it is the same endpoint.
    ///
    /// Read-only and text-only: the response changes no amount; it merely
    /// names codes to ask the owner about.
    TinvestContract,
}

impl Destination {
    /// All destinations. Exists for tests that walk the complete table:
    /// a test listing variants manually would become stale silently.
    pub const ALL: [Self; 7] = [
        Self::TinkoffProd,
        Self::TinkoffSandbox,
        Self::FinamApi,
        Self::MoexIss,
        Self::CbrScripts,
        Self::CbrDailyInfo,
        Self::TinvestContract,
    ];

    /// Endpoint base.
    ///
    /// Values are checked against `crates/iaam-broker/src/environment.rs:53`
    /// and `crates/iaam-broker/src/finam/client.rs`. The gateway domain is
    /// `tbank.ru`, not `tinkoff.ru`.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::TinkoffProd => "https://invest-public-api.tbank.ru/rest",
            Self::TinkoffSandbox => "https://sandbox-invest-public-api.tbank.ru/rest",
            Self::FinamApi => "https://api.finam.ru",
            Self::MoexIss => "https://iss.moex.com",
            Self::CbrScripts | Self::CbrDailyInfo => "https://www.cbr.ru",
            Self::TinvestContract => "https://raw.githubusercontent.com",
        }
    }
}
