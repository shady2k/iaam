//! Where a security is kept, and where the identifier for it came from.

/// Where a place of custody came from.
///
/// The two are not variants of one thing that happen to differ; they answer
/// different questions and only one of them may be offered to a document
/// reader by its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CustodyOrigin {
    /// A place of the owner's: named by him, or derived from the institution
    /// that keeps one of his accounts. It has a title he would recognise, and
    /// a document naming that title reaches it.
    Declared,
    /// A bare handle a channel produced — the broker's `positionUid`, or its
    /// own account identifier. It is not a place. The row exists so the
    /// journal's foreign key holds, and for nothing else: it is never offered
    /// by title, and never shown as one of his places.
    ///
    /// The marker is also what makes `iaam-xep0` addressable later: it names
    /// exactly the rows a repair would have to retract.
    Minted,
}

impl CustodyOrigin {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Minted => "minted",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "declared" => Some(Self::Declared),
            "minted" => Some(Self::Minted),
            _ => None,
        }
    }
}
