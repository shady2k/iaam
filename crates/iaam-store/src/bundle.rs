//! Archive bundle (§14).
//!
//! **A copy of the database file is not a complete backup**, and exporting only
//! events is even less so: it would produce different projections because the
//! set of scopes and reference data would remain outside. The bundle carries everything
//! needed to repeat the calculation: events, accounts, scope versions,
//! the reference data an event's legs point at, and the schema version under
//! which all this is recorded.
//!
//! `event_legs.custody` and `event_legs.instrument` are real, undeferred
//! foreign keys, so a bundle that carried only events, accounts and contours
//! could not be restored into an empty database at all — the first security
//! leg would be refused. `custody_places` and `instruments` close that: an
//! archive is needed exactly when the outside world is not available, and one
//! that needs a market sync before it will restore is not an archive.
//!
//! What is not yet included in the stage 1 bundle, and why: market data and
//! rates beyond the instrument catalogue itself (to be added in E3), tax
//! context (E5), and classification rules (E2). Each of these sections will
//! be added to the bundle with its own epic, and that is what the format
//! version is for.

use std::collections::HashSet;

use iaam_core::custody::CustodyOrigin;
use iaam_core::event::Event;
use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::journal::find_duplicate;
use crate::journal::write::insert_event_in;
use crate::{SqliteStore, StoreError};

/// Bundle format version.
///
/// A bundle with a newer version is not read: silently skipping an unknown
/// section would result in data loss. Version 2 adds the reference data an
/// event's legs point at — without it a bundle holding one security leg
/// could not be restored into an empty database at all.
pub const BUNDLE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContourSection {
    pub contour: uuid::Uuid,
    pub version: u32,
    pub title: String,
    pub accounts: Vec<uuid::Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
}

/// A place of custody, carried the way `custody_places` stores it.
///
/// `origin` is the raw code (`"declared"` or `"minted"`), not
/// [`CustodyOrigin`] itself: a bundle is a wire format, and the schema's
/// `CHECK` constraint — not this type — is what refuses a third value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustodyPlaceSection {
    pub id: uuid::Uuid,
    pub title: String,
    pub institution: Option<String>,
    pub origin: String,
}

/// An instrument, carried the way `instruments` stores it.
///
/// `kind` is the raw code, for the same reason `origin` above is: the
/// schema, not this type, is where an invalid code is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentSection {
    pub id: uuid::Uuid,
    pub kind: Option<String>,
    pub symbol: String,
    pub title: String,
    pub denomination_currency: String,
    pub settlement_currency: String,
    pub quote_currency: String,
    pub lineage_parent: Option<uuid::Uuid>,
    pub lineage_reason: Option<String>,
}

/// The complete bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_version: u32,
    pub schema_version: u32,
    pub exported_at: String,
    pub owner: OwnerId,
    pub events: Vec<Event>,
    pub accounts: Vec<AccountSection>,
    pub contours: Vec<ContourSection>,
    /// Every `declared` custody place of the owner's, and the `minted` ones
    /// the exported events reach. `#[serde(default)]` is what lets an
    /// archive written before this field existed still deserialize.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custody_places: Vec<CustodyPlaceSection>,
    /// The instrument closure the exported events reach, plus `lineage_parent`
    /// transitively. See the note above `custody_places`: the same
    /// `#[serde(default)]` reasoning applies here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruments: Vec<InstrumentSection>,
    /// Content checksum. Computed from the canonical
    /// representation of all sections except the checksum itself.
    pub checksum: String,
}

/// Bundle contents without metadata fields. Exists for the checksum:
/// it must be computed over everything the bundle carries, and only that.
///
/// **Every field added to [`Bundle`] must be added here too, with the same
/// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` pair on the
/// `Vec` fields.** ciborium writes a struct as a definite-length, named-key
/// map, and serde-derive computes that length after skipping — so a section
/// nobody wrote emits no bytes, shifts no neighbour, and does not move the
/// map header. `#[serde(default)]` on `Bundle` alone fixes reading an old
/// archive and leaves this checksum wrong; `skip_serializing_if` here alone
/// fixes the checksum and leaves old archives undeserialisable. Both structs,
/// or neither claim holds.
#[derive(Debug, Serialize)]
struct BundleContent<'a> {
    bundle_version: u32,
    schema_version: u32,
    owner: OwnerId,
    events: &'a [Event],
    accounts: &'a [AccountSection],
    contours: &'a [ContourSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    custody_places: &'a [CustodyPlaceSection],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    instruments: &'a [InstrumentSection],
}

impl Bundle {
    /// Content checksum.
    ///
    /// Computed from the **canonical serialization of all contents**.
    /// The first revision hashed only event identifiers and hashes
    /// of the raw data—at such a checksum, a substituted monetary value passed
    /// validation, and a corrupted archive appeared intact. This is exactly the
    /// failure the checksum exists to prevent (§14).
    ///
    /// The export date is not included in the checksum: it describes the export,
    /// not the facts being transferred, and changing it corrupts nothing.
    #[must_use]
    pub fn compute_checksum(&self) -> String {
        let content = BundleContent {
            bundle_version: self.bundle_version,
            schema_version: self.schema_version,
            owner: self.owner,
            events: &self.events,
            accounts: &self.accounts,
            contours: &self.contours,
            custody_places: &self.custody_places,
            instruments: &self.instruments,
        };
        let mut body = Vec::new();
        ciborium::into_writer(&content, &mut body)
            .unwrap_or_else(|error| panic!("bundle cannot be serialized: {error}"));
        let digest = Sha256::digest(&body);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Every `(table, column)` pair in `0001_schema.sql` where a table whose
/// name starts with `event` declares `REFERENCES instruments`.
///
/// This is the ground truth [`REFERENCE_CLOSURE_SQL`] promises to cover, the
/// way `journal::CUSTODY_REFERENCE_COLUMNS` is the ground truth
/// `Event::referenced_custodies` promises to cover. Re-exported so that
/// `tests/schema_coverage.rs` — an integration test, outside this crate's
/// own boundary — can hold the schema to it: growing this list without
/// growing the query is exactly the drift that test exists to catch. Ten
/// pairs across eight tables today; `event_corporate_action` alone carries
/// three.
pub const INSTRUMENT_REFERENCE_COLUMNS: &[(&str, &str)] = &[
    ("event_control_assertion", "instrument"),
    ("event_corporate_action", "instrument"),
    ("event_corporate_action", "predecessor"),
    ("event_corporate_action", "successor"),
    ("event_income", "instrument"),
    ("event_legs", "instrument"),
    ("event_offer_exercise", "instrument"),
    ("event_opening_position", "instrument"),
    ("event_trade", "instrument"),
    ("event_valuation", "instrument"),
];

/// The instruments an owner's events reach.
///
/// Public so `tests/schema_coverage.rs` can check it against the schema: the
/// query is a list of tables, and a list is what goes stale when a ninth
/// table gains an instrument column. Every `(table, column)` pair here is one
/// listed in [`INSTRUMENT_REFERENCE_COLUMNS`]; the two are kept in sync by
/// that test, not by construction, the same way the custody guard in that
/// file holds `journal::CUSTODY_REFERENCE_COLUMNS` to the schema without
/// re-deriving `Event::referenced_custodies` from it.
///
/// `lineage_parent` is walked transitively in the final `UNION`, because an
/// instrument's row alone does not explain itself without its parent — and
/// aliases do not travel: nothing here reaches `instrument_aliases`, because
/// the events carry resolved `InstrumentId`s, not codes.
pub const REFERENCE_CLOSURE_SQL: &str = "
    WITH RECURSIVE reached(id) AS (
        SELECT l.instrument FROM event_legs l
          JOIN events e ON e.id = l.event
         WHERE e.owner = ?1 AND l.instrument IS NOT NULL
        UNION SELECT t.instrument FROM event_trade t
          JOIN events e ON e.id = t.event WHERE e.owner = ?1
        UNION SELECT i.instrument FROM event_income i
          JOIN events e ON e.id = i.event WHERE e.owner = ?1 AND i.instrument IS NOT NULL
        UNION SELECT p.instrument FROM event_opening_position p
          JOIN events e ON e.id = p.event WHERE e.owner = ?1
        UNION SELECT v.instrument FROM event_valuation v
          JOIN events e ON e.id = v.event WHERE e.owner = ?1
        UNION SELECT c.instrument FROM event_control_assertion c
          JOIN events e ON e.id = c.event WHERE e.owner = ?1 AND c.instrument IS NOT NULL
        UNION SELECT a.instrument FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.instrument IS NOT NULL
        UNION SELECT a.predecessor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.predecessor IS NOT NULL
        UNION SELECT a.successor FROM event_corporate_action a
          JOIN events e ON e.id = a.event WHERE e.owner = ?1 AND a.successor IS NOT NULL
        UNION SELECT o.instrument FROM event_offer_exercise o
          JOIN events e ON e.id = o.event WHERE e.owner = ?1 AND o.instrument IS NOT NULL
        UNION SELECT n.lineage_parent FROM instruments n
          JOIN reached r ON n.id = r.id WHERE n.lineage_parent IS NOT NULL
    )
    SELECT n.id, n.kind, n.symbol, n.title,
           n.denomination_currency, n.settlement_currency, n.quote_currency,
           n.lineage_parent, n.lineage_reason
      FROM instruments n JOIN reached r ON n.id = r.id
     ORDER BY n.symbol, n.id
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// How many events were added and how many already existed.
    Applied { inserted: usize, duplicates: usize },
}

impl SqliteStore {
    /// Bundle export.
    pub fn export_bundle(&self, owner: OwnerId) -> Result<Bundle, StoreError> {
        let events = self.load_events(owner)?;
        let accounts = self
            .list_accounts(owner)?
            .into_iter()
            .map(|record| AccountSection {
                id: record.id.inner(),
                title: record.title,
                institution: record.institution,
            })
            .collect();

        let mut statement = self.conn.prepare(
            "SELECT v.contour, v.version, v.title, a.account
             FROM contour_versions v
             LEFT JOIN contour_accounts a
               ON a.owner = v.owner AND a.contour = v.contour AND a.version = v.version
             WHERE v.owner = ?1
             ORDER BY v.contour, v.version, a.account",
        )?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut contours: Vec<ContourSection> = Vec::new();
        for row in rows {
            let (contour, version, title, account) = row?;
            let contour = parse(&contour, "contour")?;
            let account = account
                .map(|value| parse(&value, "contour_account"))
                .transpose()?;
            match contours
                .last_mut()
                .filter(|section| section.contour == contour && section.version == version)
            {
                Some(section) => section.accounts.extend(account),
                None => contours.push(ContourSection {
                    contour,
                    version,
                    title,
                    accounts: account.into_iter().collect(),
                }),
            }
        }

        // A minted row is as load bearing for the foreign key as a declared
        // one, so every one the exported events reach travels alongside
        // every declared row of the owner's. Scoping the minted half to
        // reachability also keeps a failed synchronization's unreferenced
        // handles out of the archive — the write order (T6) registers a
        // handle before it appends, so an append that failed leaves one
        // behind with nothing pointing at it.
        let reached_custodies: HashSet<uuid::Uuid> = events
            .iter()
            .flat_map(Event::referenced_custodies)
            .map(|id| id.inner())
            .collect();
        let custody_places = self
            .list_custody_places(owner)?
            .into_iter()
            .filter(|place| {
                place.origin == CustodyOrigin::Declared
                    || reached_custodies.contains(&place.id.inner())
            })
            .map(|place| CustodyPlaceSection {
                id: place.id.inner(),
                title: place.title,
                institution: place.institution,
                origin: place.origin.code().to_owned(),
            })
            .collect();

        // The closure reached from the exported events, plus `lineage_parent`
        // transitively — not the whole directory, which a market sync fills
        // with an exchange's entire universe and an archive of one owner's
        // affairs has no business carrying.
        let mut statement = self.conn.prepare(REFERENCE_CLOSURE_SQL)?;
        let rows = statement.query_map([owner.inner().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        let mut instruments = Vec::new();
        for row in rows {
            let (
                id,
                kind,
                symbol,
                title,
                denomination,
                settlement,
                quote,
                lineage_parent,
                lineage_reason,
            ) = row?;
            let lineage_parent = lineage_parent
                .map(|value| parse(&value, "instrument"))
                .transpose()?;
            instruments.push(InstrumentSection {
                id: parse(&id, "instrument")?,
                kind,
                symbol,
                title,
                denomination_currency: denomination,
                settlement_currency: settlement,
                quote_currency: quote,
                lineage_parent,
                lineage_reason,
            });
        }

        let mut bundle = Bundle {
            bundle_version: BUNDLE_VERSION,
            schema_version: crate::schema::SCHEMA_VERSION,
            exported_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z")),
            owner,
            events,
            accounts,
            contours,
            custody_places,
            instruments,
            checksum: String::new(),
        };
        bundle.checksum = bundle.compute_checksum();
        Ok(bundle)
    }

    /// Bundle import.
    ///
    /// Idempotent: events with known keys are not created again.
    /// Runs in **a single transaction**: a partially imported archive
    /// is a state that never existed, and dealing with it is worse
    /// than dealing with a failed import.
    ///
    /// Rejects a bundle that is: newer than the supported format; written
    /// with a schema newer than supported; inconsistent with the checksum;
    /// contains events belonging to another owner.
    pub fn import_bundle(&mut self, bundle: &Bundle) -> Result<ImportOutcome, StoreError> {
        if bundle.bundle_version > BUNDLE_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.bundle_version,
                supported: BUNDLE_VERSION,
            });
        }
        if bundle.schema_version > crate::schema::SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: bundle.schema_version,
                supported: crate::schema::SCHEMA_VERSION,
            });
        }
        if bundle.checksum != bundle.compute_checksum() {
            return Err(StoreError::BundleCorrupted {
                detail: "checksum does not match contents".into(),
            });
        }
        // There is one owner in a bundle. An event belonging to another owner means either
        // that two archives were joined or that a substitution occurred; either would make
        // the ownership boundary fictitious (§14).
        if let Some(foreign) = bundle
            .events
            .iter()
            .find(|event| event.owner != bundle.owner)
        {
            return Err(StoreError::BundleCorrupted {
                detail: format!("event {} belongs to another owner", foreign.id.inner()),
            });
        }

        let owner = bundle.owner;
        let created_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"));
        let transaction = self.conn.transaction()?;

        for account in &bundle.accounts {
            transaction.execute(
                "INSERT INTO accounts (id, owner, title, institution, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE accounts.owner = excluded.owner",
                params![
                    account.id.to_string(),
                    owner.inner().to_string(),
                    account.title,
                    account.institution,
                    created_at,
                ],
            )?;
        }

        // Before the events loop: `PRAGMA foreign_keys` is on
        // (`store/src/lib.rs:239`) and no constraint in this schema is
        // deferred, so SQLite checks at the statement that violates it, not
        // at COMMIT. Order here is a requirement, not a habit.
        for place in &bundle.custody_places {
            transaction.execute(
                "INSERT INTO custody_places (id, owner, title, institution, origin, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET
                     title = excluded.title,
                     institution = excluded.institution
                 WHERE custody_places.owner = excluded.owner",
                params![
                    place.id.to_string(),
                    owner.inner().to_string(),
                    place.title,
                    place.institution,
                    place.origin,
                    created_at,
                ],
            )?;
        }

        // An archive supplies what is missing and never overwrites what is
        // there. Instruments are global reference data shared with whatever
        // else this database holds, so a restore must not silently rewrite a
        // live directory with a snapshot of an older one. Accounts and
        // custody places are the owner's own and do update, above.
        //
        // `instruments.lineage_parent` is a self-referencing foreign key, so
        // an instrument whose parent is also in this section must be
        // inserted after it. The closure query includes every such parent
        // transitively, but does not order by lineage, so this inserts in
        // passes: each pass inserts every remaining instrument whose parent
        // is not itself still pending, and stops once a pass inserts
        // nothing — which happens exactly when every instrument is in.
        let mut pending: Vec<&InstrumentSection> = bundle.instruments.iter().collect();
        let mut placed: HashSet<uuid::Uuid> = HashSet::new();
        while !pending.is_empty() {
            let mut still_pending = Vec::new();
            let mut progressed = false;
            for instrument in pending {
                let parent_still_pending = instrument.lineage_parent.is_some_and(|parent| {
                    bundle
                        .instruments
                        .iter()
                        .any(|candidate| candidate.id == parent)
                        && !placed.contains(&parent)
                });
                if parent_still_pending {
                    still_pending.push(instrument);
                    continue;
                }
                transaction.execute(
                    "INSERT INTO instruments
                         (id, kind, symbol, title,
                          denomination_currency, settlement_currency, quote_currency,
                          lineage_parent, lineage_reason, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT (id) DO NOTHING",
                    params![
                        instrument.id.to_string(),
                        instrument.kind,
                        instrument.symbol,
                        instrument.title,
                        instrument.denomination_currency,
                        instrument.settlement_currency,
                        instrument.quote_currency,
                        instrument.lineage_parent.map(|id| id.to_string()),
                        instrument.lineage_reason,
                        created_at,
                    ],
                )?;
                placed.insert(instrument.id);
                progressed = true;
            }
            if !progressed {
                // Every instrument left names a parent also left, in this
                // section: a cycle, which nothing in this codebase writes.
                // Refusing beats looping forever on an archive that cannot
                // exist honestly.
                return Err(StoreError::BundleCorrupted {
                    detail: "instrument lineage_parent forms a cycle within one bundle section"
                        .into(),
                });
            }
            pending = still_pending;
        }

        for contour in &bundle.contours {
            // The scope version is immutable: an existing one is skipped,
            // rather than overwritten.
            let known: Option<u32> = transaction
                .query_row(
                    "SELECT version FROM contour_versions
                     WHERE owner = ?1 AND contour = ?2 AND version = ?3",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                continue;
            }
            transaction.execute(
                "INSERT INTO contour_versions (owner, contour, version, title, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    owner.inner().to_string(),
                    contour.contour.to_string(),
                    contour.version,
                    contour.title,
                    created_at,
                ],
            )?;
            for account in &contour.accounts {
                transaction.execute(
                    "INSERT INTO contour_accounts (owner, contour, version, account)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        owner.inner().to_string(),
                        contour.contour.to_string(),
                        contour.version,
                        account.to_string(),
                    ],
                )?;
            }
        }

        let mut inserted = 0;
        let mut duplicates = 0;
        for event in &bundle.events {
            if find_duplicate(
                &transaction,
                event,
                iaam_core::reconciliation::evidence::IdentityScope::Source,
            )?
            .is_some()
            {
                duplicates += 1;
                continue;
            }
            insert_event_in(&transaction, event)?;
            inserted += 1;
        }

        transaction.commit()?;
        Ok(ImportOutcome::Applied {
            inserted,
            duplicates,
        })
    }
}

fn parse(value: &str, what: &'static str) -> Result<uuid::Uuid, StoreError> {
    uuid::Uuid::parse_str(value).map_err(|_| StoreError::NotFound {
        what,
        id: value.to_owned(),
    })
}
