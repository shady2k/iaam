//! The operation kind named by a broker channel.
//!
//! One type is intentional across all channels. Previously,
//! `ChannelOperationKind` was declared twice—once for T-Invest and once for
//! Finam—and two enums with the same meaning could silently diverge: adding a
//! member to one channel did not stop the other from compiling, and the
//! difference surfaced not at build time but when one broker's operation was
//! converted into something different from another broker's.
//!
//! **This is a vocabulary of meanings, not broker codes.** Each channel has
//! its own open-ended set of codes that changes without our involvement, so
//! the mapping “source code → enum member” lives in data, not in a `match`
//! (epic iaam-d8b.2.2). This enum lists what the system can do with an
//! operation, and every new member must break compilation wherever parsing is
//! incomplete (§15.1).

/// What the channel reported about the operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOperationKind {
    /// Instrument purchase.
    Buy,
    /// Instrument sale.
    Sell,
    /// Dividend payment.
    Dividend,
    /// Coupon payment.
    Coupon,
    /// Broker or service fee.
    Commission,
    /// Account deposit.
    Deposit,
    /// Withdrawal of money or securities.
    Withdrawal,
    /// Transfer between accounts or custodians.
    Transfer,
    /// Bond amortisation: outstanding principal decreases, cash arrives,
    /// and the number of securities does not change (§6.5).
    ///
    /// A separate member, not income: income does not reduce principal, and
    /// treating amortisation as income would overstate both income and the
    /// position's cost.
    BondAmortisation,
    /// Final bond redemption: principal is returned in full and the
    /// security leaves the position.
    BondRedemption,
    /// Kind absent from the channel dictionary.
    ///
    /// A string, not a parse refusal: the owner needs the kind's name,
    /// otherwise the refusal would not say what the system does not know.
    Other(String),
}

impl ChannelOperationKind {
    /// Kind name in the dictionary and schema.
    ///
    /// `Other` intentionally has no name: “kind unknown” is expressed by
    /// the absence of a dictionary entry, not by a string named “other”.
    /// Recording “other” would mean deciding not to parse it, and no such
    /// decision was made.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::Buy => Some("buy"),
            Self::Sell => Some("sell"),
            Self::Dividend => Some("dividend"),
            Self::Coupon => Some("coupon"),
            Self::Commission => Some("commission"),
            Self::Deposit => Some("deposit"),
            Self::Withdrawal => Some("withdrawal"),
            Self::Transfer => Some("transfer"),
            Self::BondAmortisation => Some("bond_amortisation"),
            Self::BondRedemption => Some("bond_redemption"),
            Self::Other(_) => None,
        }
    }

    /// Parse a dictionary name.
    ///
    /// An unknown name returns `None`, not `Other`: `Other` means “the
    /// channel sent a code absent from the dictionary”, while this means the
    /// dictionary contains a kind unknown to this build. Merging them would
    /// hide a schema/code mismatch behind an ordinary unknown broker code.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "buy" => Some(Self::Buy),
            "sell" => Some(Self::Sell),
            "dividend" => Some(Self::Dividend),
            "coupon" => Some(Self::Coupon),
            "commission" => Some(Self::Commission),
            "deposit" => Some(Self::Deposit),
            "withdrawal" => Some(Self::Withdrawal),
            "transfer" => Some(Self::Transfer),
            "bond_amortisation" => Some(Self::BondAmortisation),
            "bond_redemption" => Some(Self::BondRedemption),
            _ => None,
        }
    }
}

/// One channel's dictionary: how its codes become operation kinds.
///
/// Built from storage data and passed to the parser as a parameter. The broker
/// crate intentionally knows nothing about storage (see `lib.rs`), so the
/// application adapter connects them using the same approach as SQLite.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationKindDictionary {
    entries: std::collections::BTreeMap<String, ChannelOperationKind>,
}

/// A dictionary row that this build could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableEntry {
    pub source_kind: String,
    pub kind: String,
}

impl OperationKindDictionary {
    /// Build a dictionary from “channel code → kind name” pairs.
    ///
    /// Unreadable rows are returned alongside the dictionary rather than
    /// discarded: a row the build cannot understand means the database is
    /// newer than the code. Silently ignoring it would turn a known broker
    /// code into an unknown one—an import refusal with no explanation.
    #[must_use]
    pub fn build<I, K, V>(rows: I) -> (Self, Vec<UnreadableEntry>)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: AsRef<str>,
    {
        let mut entries = std::collections::BTreeMap::new();
        let mut unreadable = Vec::new();
        for (source_kind, kind) in rows {
            let source_kind = source_kind.into();
            match ChannelOperationKind::parse(kind.as_ref()) {
                Some(parsed) => {
                    entries.insert(source_kind, parsed);
                }
                None => unreadable.push(UnreadableEntry {
                    source_kind,
                    kind: kind.as_ref().to_owned(),
                }),
            }
        }
        (Self { entries }, unreadable)
    }

    /// What the channel turned its code into.
    ///
    /// A code absent from the dictionary becomes `Other` containing the code:
    /// the refusal must name what the system does not know.
    #[must_use]
    pub fn kind_of(&self, source_kind: &str) -> ChannelOperationKind {
        self.entries
            .get(source_kind)
            .cloned()
            .unwrap_or_else(|| ChannelOperationKind::Other(source_kind.to_owned()))
    }

    /// Number of codes known to the dictionary.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the dictionary is empty.
    ///
    /// An empty dictionary does not mean “the broker sent something unknown”;
    /// it means “the dictionary was not seeded”. The caller must distinguish
    /// them, otherwise the owner gets a broker refusal instead of a
    /// configuration refusal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Initial channel dictionary selected by broker code.
///
/// Returns `None` for a broker about which there is no knowledge: an empty
/// table would mean “this channel has no operation kinds”, which is a
/// different claim, and access setup would accept it silently.
#[must_use]
pub fn seed_for(broker: &str) -> Option<(&'static str, &'static [(&'static str, &'static str)])> {
    match broker {
        "tinkoff" => Some((
            crate::tinkoff::dictionary_seed::TINKOFF_SEED_NAME,
            crate::tinkoff::dictionary_seed::TINKOFF_OPERATION_KINDS,
        )),
        "finam" => Some((
            crate::finam::dictionary_seed::FINAM_SEED_NAME,
            crate::finam::dictionary_seed::FINAM_OPERATION_KINDS,
        )),
        _ => None,
    }
}
