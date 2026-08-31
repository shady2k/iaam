//! Event construction for core integration tests.
//!
//! This lives in a separate module because the crate-internal `test_support`
//! is available only to unit tests: an integration test is an external
//! consumer and must construct an event through the public interface.

use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
use iaam_core::event::kind::EventKind;
use iaam_core::event::leg::Leg;
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use time::Date;

/// Data ingestion channel in tests: source, parser version, document.
///
/// This is a separate entity rather than three arguments because
/// the **source is part of the channel identity**. Giving each event
/// its own random `SourceId` means splitting one document into as many
/// channels as it has rows—and no basis requiring
/// multiple sections of the same document will work (§10.3).
pub struct TestChannel {
    pub(crate) source: SourceId,
    pub(crate) parser: ParserVersion,
    pub(crate) document: RawHash,
}

impl TestChannel {
    /// One document parsed by one parser.
    #[must_use]
    pub fn new(parser: &str, document: &str) -> Self {
        Self {
            source: SourceId::new_random(),
            parser: ParserVersion(parser.to_owned()),
            document: document_hash(document),
        }
    }
    fn provenance(&self) -> Provenance {
        Provenance::new(self.source, self.document.clone(), self.parser.clone())
    }
}

/// A document hash derived from a human-readable name.
///
/// The name is hex-encoded and padded to sixty-four
/// characters: `RawHash` accepts only a valid SHA-256, while tests need
/// documents that are distinct and recognizable in debug output, not real hashes.
#[must_use]
pub fn document_hash(name: &str) -> RawHash {
    let mut hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
    assert!(hex.len() <= 64, "document name {name} is too long");
    while hex.len() < 64 {
        hex.push('0');
    }
    RawHash::parse(&hex).expect("hexadecimal hash")
}

/// Where and when an event is recorded.
///
/// Packaged as a struct rather than four arguments: in a helper that
/// takes two identifiers, a date, and a number in sequence, swapping
/// arguments is easy, but noticing it is not.
#[derive(Debug, Clone, Copy)]
pub struct Posting {
    pub owner: OwnerId,
    pub account: AccountId,
    pub day: Date,
    pub sequence: u32,
}

/// An event received through the specified channel.
#[must_use]
pub fn event_on(channel: &TestChannel, posting: Posting, kind: EventKind, legs: Vec<Leg>) -> Event {
    Event {
        id: EventId::new_random(),
        schema_version: SCHEMA_VERSION,
        owner: posting.owner,
        account: posting.account,
        kind,
        dates: EventDates::for_cash(CashPostedDate(posting.day)),
        order: EffectiveOrder::new(posting.day, posting.sequence),
        legs,
        provenance: channel.provenance(),
        relation: Relation::None,
        confidence: Confidence::Known,
        idempotency_key: None,
    }
}
