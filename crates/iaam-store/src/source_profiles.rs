//! What content each source profile version names (decision 0019 §5).
//!
//! A profile's version is a name for a content, and every fact the profile
//! reads records that name as its `ParserVersion`. The claim that makes the
//! name worth anything is that «the rows version 3 read» is a **set** — one
//! query, answerable without archaeology, which is what lets a buggy profile's
//! facts be found and retracted. That claim is false the moment one pair names
//! two contents.
//!
//! Nothing in the profile catalogue can keep it true on its own. The catalogue
//! is assembled at start-up and refuses two files claiming one id **within that
//! one pass**; a file edited between two starts is compared against nothing.
//! So the binding lives here, in the one part of the instance that outlives the
//! process, and the catalogue asks this table on every load.
//!
//! **The table is instance-wide and not per owner**, for the reason written on
//! `ProfileCatalogue` itself: the catalogue is a property of the deployment
//! rather than of a journal, because two instances of one image must read one
//! institution's export the same way.

use rusqlite::{OptionalExtension, params};

use crate::{SqliteStore, StoreError, now};

/// What this instance already recorded under one `(id, version)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileBinding {
    /// Nothing stood under the pair, and this content now does.
    Recorded,
    /// The pair already stood for this content, and still does.
    Unchanged,
    /// The pair stands for a different content, and this one is not it.
    ///
    /// Carries what was recorded rather than only the fact of the mismatch: a
    /// caller told «refused» and nothing else can only compare files by hand.
    Differs { recorded: String },
}

/// One `(id, version)` and the content it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProfileVersion {
    pub id: String,
    pub version: u32,
    pub digest: String,
}

impl SqliteStore {
    /// Bind this content to this `(id, version)`, or report what is bound
    /// already.
    ///
    /// **This never overwrites.** The row that stands is the one that got there
    /// first, so the answer to a second content is `Differs` and not a silent
    /// replacement — a table that rewrote the digest under a standing pair
    /// would perform the very defect it exists to catch.
    ///
    /// Two statements and no transaction, which is safe here precisely because
    /// of that: a row of this table is written once and never updated or
    /// deleted, so between the insert and the read there is nothing another
    /// writer could change. The insert reports whether it was the one that
    /// wrote the row, which is what tells a first load from a repeat one.
    pub fn bind_source_profile_version(
        &self,
        id: &str,
        version: u32,
        digest: &str,
    ) -> Result<ProfileBinding, StoreError> {
        let written = self.conn.execute(
            "INSERT INTO source_profile_versions (id, version, digest, first_loaded_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id, version) DO NOTHING",
            params![id, version, digest, now()],
        )?;
        if written == 1 {
            return Ok(ProfileBinding::Recorded);
        }
        let recorded: Option<String> = self
            .conn
            .query_row(
                "SELECT digest FROM source_profile_versions WHERE id = ?1 AND version = ?2",
                params![id, version],
                |row| row.get(0),
            )
            .optional()?;
        // A pair the insert did not write is a pair that stands. Its absence a
        // moment later would mean somebody deleted it, which nothing here does;
        // treating that as «nothing recorded» would install the content the
        // deletion was hiding, so it is reported as the storage fault it is.
        let recorded = recorded.ok_or_else(|| StoreError::NotFound {
            what: "source profile version",
            id: format!("{id}/{version}"),
        })?;
        if recorded == digest {
            Ok(ProfileBinding::Unchanged)
        } else {
            Ok(ProfileBinding::Differs { recorded })
        }
    }

    /// Every binding this instance has recorded, oldest pair first.
    ///
    /// For an operator holding a refusal: the pair it names is here beside the
    /// ones that still load, so «which version was this content ever under» is
    /// a question the instance answers rather than one he answers with `git`.
    pub fn list_source_profile_versions(&self) -> Result<Vec<SourceProfileVersion>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, version, digest
             FROM source_profile_versions
             ORDER BY id, version",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SourceProfileVersion {
                id: row.get(0)?,
                version: row.get(1)?,
                digest: row.get(2)?,
            })
        })?;
        let mut bound = Vec::new();
        for entry in rows {
            bound.push(entry?);
        }
        Ok(bound)
    }
}
