//! Archive bundle (§14).
//!
//! **A copy of the database file is not a complete backup**, and exporting only
//! events is even less so: it would produce different projections because the
//! set of scopes and reference data would remain outside. The bundle carries everything
//! needed to repeat the calculation: events, accounts, scope versions,
//! and the schema version under which all this is recorded.
//!
//! What is not yet included in the stage 1 bundle, and why: market data and rates
//! (to be added in E3), tax context (E5), and classification rules
//! (E2). Each of these sections will be added to the bundle with its own epic,
//! and that is what the format version is for.

use iaam_core::event::Event;
use iaam_core::ids::OwnerId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::events::{find_duplicate, insert_event};
use crate::{SqliteStore, StoreError};

/// Bundle format version. A bundle with a newer version is not read:
/// silently skipping an unknown section would result in data loss.
pub const BUNDLE_VERSION: u32 = 1;

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
    /// Content checksum. Computed from the canonical
    /// representation of all sections except the checksum itself.
    pub checksum: String,
}

/// Bundle contents without metadata fields. Exists for the checksum:
/// it must be computed over everything the bundle carries, and only that.
#[derive(Debug, Serialize)]
struct BundleContent<'a> {
    bundle_version: u32,
    schema_version: u32,
    owner: OwnerId,
    events: &'a [Event],
    accounts: &'a [AccountSection],
    contours: &'a [ContourSection],
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
        };
        let mut body = Vec::new();
        ciborium::into_writer(&content, &mut body)
            .unwrap_or_else(|error| panic!("bundle cannot be serialized: {error}"));
        let digest = Sha256::digest(&body);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

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
            insert_event(&transaction, event)?;
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
