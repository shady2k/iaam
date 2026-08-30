//! Instrument catalogue: kind, aliases, and currency roles (§4.5, §5.4, §7.2).
//!
//! Only immutable instrument properties belong here. The valuation-policy
//! sentence in §5.4 also depends on whether a price exists and how old it is
//! on the relevant date, so E3.3 derives it rather than storing a column.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::ids::InstrumentId;
use crate::money::CurrencyCode;

/// Instrument kind. An immutable property: a share does not become a bond.
///
/// This is an exhaustive `enum`, following [`CurrencyCode`]: adding a kind
/// must break compilation everywhere it is not handled (§15.1).
///
/// `Futures` and `Option` are intentionally absent: §11 puts derivatives
/// outside the perimeter together with shorts, margin, and repos, and no
/// liability ledger is built. `Deposit` is absent for a different reason: a
/// deposit is an account, not an instrument—it has neither quantity nor a
/// place of custody (§4.5; the `AccountId` doc comment explicitly calls it a
/// cash account).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InstrumentKind {
    Share,
    DepositaryReceipt,
    Bond,
    /// Exchange-traded fund: it has a quote.
    Etf,
    /// Mutual fund: unit net asset value, not a quote.
    MutualFund,
    Currency,
    Crypto,
    RealEstate,
    PrivateShare,
    Loan,
}

impl InstrumentKind {
    /// All variants. This exists for table-driven tests: a list assembled by
    /// hand in a test would silently drift from the `enum`.
    pub const ALL: [Self; 10] = [
        Self::Share,
        Self::DepositaryReceipt,
        Self::Bond,
        Self::Etf,
        Self::MutualFund,
        Self::Currency,
        Self::Crypto,
        Self::RealEstate,
        Self::PrivateShare,
        Self::Loan,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::DepositaryReceipt => "depositary_receipt",
            Self::Bond => "bond",
            Self::Etf => "etf",
            Self::MutualFund => "mutual_fund",
            Self::Currency => "currency",
            Self::Crypto => "crypto",
            Self::RealEstate => "real_estate",
            Self::PrivateShare => "private_share",
            Self::Loan => "loan",
        }
    }

    /// Parse a code. `None`, rather than a default, ensures an unknown kind
    /// reaches the caller instead of becoming a share (§4.9).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Namespace of an external instrument code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AliasNamespace {
    Isin,
    MoexSecid,
    Ticker,
    Figi,
    /// Internal broker code: different brokers use different codes for one security.
    BrokerCode,
}

impl AliasNamespace {
    pub const ALL: [Self; 5] = [
        Self::Isin,
        Self::MoexSecid,
        Self::Ticker,
        Self::Figi,
        Self::BrokerCode,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Isin => "isin",
            Self::MoexSecid => "moex_secid",
            Self::Ticker => "ticker",
            Self::Figi => "figi",
            Self::BrokerCode => "broker_code",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|namespace| namespace.code() == code)
    }
}

/// Why an instrument has a predecessor (§7.2, §4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LineageReason {
    /// Replacement bond.
    Replacement,
    Conversion,
    Merger,
    SpinOff,
}

impl LineageReason {
    pub const ALL: [Self; 4] = [
        Self::Replacement,
        Self::Conversion,
        Self::Merger,
        Self::SpinOff,
    ];

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Replacement => "replacement",
            Self::Conversion => "conversion",
            Self::Merger => "merger",
            Self::SpinOff => "spin_off",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.code() == code)
    }
}

/// Three currency roles for one instrument (§7.2).
///
/// A struct, rather than three positional `CurrencyCode` values: consecutive
/// arguments of the same type can be swapped without the compiler noticing
/// (§15.1). The reporting currency does not belong here; it is a property of
/// the report and owner, not of the security.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyRoles {
    /// Liability currency.
    pub denomination: CurrencyCode,
    /// Settlement currency.
    pub settlement: CurrencyCode,
    /// Quotation currency.
    pub quote: CurrencyCode,
}

impl CurrencyRoles {
    /// All three roles match—the usual case for a rouble-denominated security.
    #[must_use]
    pub const fn uniform(currency: CurrencyCode) -> Self {
        Self {
            denomination: currency,
            settlement: currency,
            quote: currency,
        }
    }
}

/// Validity interval for an alias.
///
/// The start is inclusive and the end exclusive. This half-open interval lets
/// adjacent intervals for one code meet without gaps or overlap: with an
/// inclusive end, the ISIN-change date would belong to two records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasInterval {
    pub valid_from: Date,
    /// `None` means an open-ended interval.
    pub valid_to: Option<Date>,
}

impl AliasInterval {
    #[must_use]
    pub fn covers(&self, on: Date) -> bool {
        on >= self.valid_from && self.valid_to.is_none_or(|end| on < end)
    }
}

/// Instrument lineage: replacement, conversion, or merger (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    pub parent: InstrumentId,
    pub reason: LineageReason,
}
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_interval_includes_its_first_day() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: None,
        };
        assert!(interval.covers(date!(2023 - 01 - 10)));
    }

    #[test]
    fn an_interval_excludes_the_day_it_ends() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: Some(date!(2024 - 05 - 20)),
        };
        assert!(interval.covers(date!(2024 - 05 - 19)));
        assert!(!interval.covers(date!(2024 - 05 - 20)));
    }

    #[test]
    fn an_open_interval_covers_every_later_day() {
        let interval = AliasInterval {
            valid_from: date!(2023 - 01 - 10),
            valid_to: None,
        };
        assert!(interval.covers(date!(2099 - 12 - 31)));
        assert!(!interval.covers(date!(2023 - 01 - 09)));
    }

    #[test]
    fn every_kind_survives_a_round_trip_through_its_code() {
        for kind in InstrumentKind::ALL {
            assert_eq!(InstrumentKind::from_code(kind.code()), Some(kind));
        }
    }

    #[test]
    fn every_namespace_survives_a_round_trip_through_its_code() {
        for namespace in AliasNamespace::ALL {
            assert_eq!(AliasNamespace::from_code(namespace.code()), Some(namespace));
        }
    }

    #[test]
    fn every_lineage_reason_survives_a_round_trip_through_its_code() {
        for reason in LineageReason::ALL {
            assert_eq!(LineageReason::from_code(reason.code()), Some(reason));
        }
    }

    #[test]
    fn an_unknown_code_is_not_guessed() {
        assert_eq!(InstrumentKind::from_code("derivative"), None);
        assert_eq!(AliasNamespace::from_code("cusip"), None);
    }
}
