//! Bundle export and restore, as local administration commands (§14).
//!
//! `SqliteStore::export_bundle` and `SqliteStore::import_bundle`
//! (`iaam-store/src/bundle.rs`) had no caller outside a test until this file.
//! An agent that needed to move an instance had to replay every fact through
//! the ingest API instead, one row at a time — 5 500 calls for one field
//! report. This module is the way in and the way out: `iaam bundle export`
//! and `iaam bundle import`.
//!
//! ## Why a command and not a route
//!
//! An export is the owner's entire financial history, encoded whole, in one
//! file. Every other read this API publishes answers a bounded question — a
//! report over an interval, a page of the journal — and §4.3 of
//! `docs/api/conventions.md` admits all three token scopes to every one of
//! them, because none of them is the instance's total: reading a report twice
//! with different arguments never adds up to reading the bundle once. The
//! owner decided what a bundle is for (iaam-k3gh.9's notes): "the
//! transferable state of an instance," not a report about it. That is what
//! makes exporting it a different act from reading a report even though both
//! are reads — and conventions §4 has no vocabulary for a read narrower than
//! the three scopes it already grants uniformly, because every read on this
//! surface sits at that same floor today. Gating one route tighter would be
//! the first exception to that rule, made once, for one endpoint, rather than
//! a considered widening of the authority model — exactly the kind of
//! decision §4.1 warns against making at the route instead of at the act.
//!
//! Decision 0003 already drew a boundary of this shape once, for a different
//! reason: it moved credential issuance off HTTP and left the operator's-eye
//! argument on record — "its authority comes from the operating system: the
//! identity it runs as, the file permissions on the database and the key...
//! A CLI invoked from somewhere those do not hold has no more authority than
//! an HTTP caller." A bundle carries no credential — token, broker access and
//! their usage log are excluded from it deliberately (see the note on
//! [`import_from_file`]) — so it does not need that decision's reasoning
//! about secrets reaching an agent's context. It does need the other half:
//! **where the file is written, and by whom.** A command run by whoever has a
//! shell on the box writes the archive straight to a path that operator
//! chose, with no HTTP body, reverse proxy, access log or rate limiter
//! standing between the database and the disk — and reads one back the same
//! way. A route would additionally need a second wire format beside this
//! API's JSON DTOs for a payload this size (§5 forbids smuggling a structure
//! through a string, and a multi-megabyte archive is not a field), and would
//! hand the owner's whole history to whatever infrastructure terminates the
//! request. Neither problem has an answer as good as the one the CLI already
//! has for free — decision 0003's own operator, running commands directly
//! against the instance he administers — so the command wins.
//!
//! ## Restoring into a database that already holds facts
//!
//! Import is transactional (`SqliteStore::import_bundle`'s own doc comment),
//! so a partial restore is not the risk. The risk this module exists to
//! close is a *complete* one landing somewhere the caller did not expect: an
//! archive from last month, restored by habit against an instance that has
//! moved on since, merges silently and nobody notices which facts came from
//! which run.
//!
//! So restoring refuses outright, before it opens the archive at all, when
//! the target already holds journal facts for its owner — this codebase's
//! own vocabulary keeps "facts" (the journal) distinct from "answers"
//! (everything else an owner has decided, per iaam-oqsw), and the journal is
//! the one the owner cannot get back by re-answering questions. `--merge`
//! overrides the refusal and nothing else does: there is no size threshold,
//! no age check, no confirmation prompt to skip past. An empty journal needs
//! no flag, because there is nothing a merge could disagree with.
//!
//! ## The owner identifier does not travel usefully on its own
//!
//! An archive's `Bundle::owner` is meaningless on a different instance: no
//! token is ever minted under it, because tokens are excluded from the
//! bundle by design (§14) and `iaam claim` always mints a fresh one. Facts
//! recorded under the archive's original owner would sit in the database
//! forever, visible to no credential this instance will ever issue — a
//! restore that "succeeded" and could not be read back, which is worse than
//! one that refused. So a restore rewrites the archive's owner, and every
//! event's, to the instance's own sole owner before writing anything, and
//! recomputes the checksum over the rewritten content — [`Bundle`]'s other
//! sections carry no owner field of their own; only [`Event`] repeats it, so
//! only events need rewriting alongside the top-level field. This is
//! reported, not silent: [`RestoreReport`] states the archive's original
//! owner whenever it differs from the one the facts were actually recorded
//! under.
//!
//! If the instance has no owner yet (`iaam claim` was never run) there is
//! nothing to attach the archive to, and guessing which owner it will
//! eventually get would be exactly the silent guess this module exists to
//! refuse — so an unclaimed instance refuses the restore and names the
//! command that unblocks it.
//!
//! ## What a bundle leaves out, surfaced rather than discovered
//!
//! A restore that carried less than everything must not look complete
//! (iaam-k3gh.9's own framing). `iaam-store` publishes exactly this as
//! [`iaam_store::bundle::TABLE_DISPOSITIONS`]: every table in the schema,
//! paired with why it does or does not travel — a credential, something
//! recomputed on restore, or something classified but not yet carried.
//! [`RestoreReport`] renders every entry that is not
//! [`TableDisposition::Carried`], by iterating that list rather than naming
//! a single table here: the list is what `tests/bundle_coverage.rs` holds to
//! the schema, so a table added to the database later is guaranteed to be
//! classified there before this report can go stale by omission.
//!
//! ## What this module cannot yet report
//!
//! [`RestoreReport::outcome`] relays [`ImportOutcome`] as `iaam-store`
//! publishes it — today that is event counts alone. Every other section
//! `import_bundle` writes (accounts, custody places, contours, and whatever
//! iaam-k3gh.9.2–.9.3 add) is written or kept without a count of its own.
//! This module does not invent a parallel count: it renders what
//! `iaam-store` reports and grows exactly as that grows, rather than
//! re-deriving section-by-section bookkeeping here that could drift from the
//! store's own the moment a new section is added there.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use iaam_core::ids::OwnerId;
use iaam_store::SqliteStore;
use iaam_store::broker_access::SoleOwner;
use iaam_store::bundle::{Bundle, ImportOutcome, TABLE_DISPOSITIONS, TableDisposition};

#[derive(Debug, thiserror::Error)]
pub enum BundleCliError {
    #[error("instance has no owner: run `iaam claim --label <label>` first")]
    Unclaimed,
    #[error(
        "multiple owners recorded in the database: choosing which one a bundle \
         belongs to is impossible. These are signs of corruption in a \
         single-user system — inspect the database, not this command"
    )]
    AmbiguousOwner,
    #[error(
        "this instance already holds journal facts; restoring would merge the \
         archive into them rather than start from an empty database. Pass \
         --merge if that is what you want"
    )]
    NotEmpty,
    #[error("{path} already exists: refusing to overwrite an existing archive")]
    OutputExists { path: String },
    #[error("{path} could not be created: {source}")]
    Create {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} could not be written: {source}")]
    Serialize {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path} could not be opened: {source}")]
    Open {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a valid bundle archive: {source}")]
    Deserialize {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    /// The archive failed its own integrity checks, read exactly as it lies on
    /// disk and before this command altered a byte of it.
    ///
    /// **This must be raised before the owner is rewritten, not after.** Both
    /// checks it stands for belong to the store and are made by
    /// `import_bundle` — the checksum, whose stated purpose (§14) is to catch a
    /// substituted monetary value, and the refusal of an archive whose events
    /// do not all belong to its own owner, whose stated purpose is to catch two
    /// archives joined into one. A restore onto another instance rewrites every
    /// event's owner and then the checksum, so by the time `import_bundle` sees
    /// the bundle both checks pass by construction: one over content this
    /// command just re-digested, the other over an owner field this command
    /// just made uniform. Verifying here, first, is what keeps them meaning
    /// what they say in the one case the whole command exists for.
    #[error("{path} is not the archive it says it is: {detail}")]
    Corrupted { path: String, detail: String },
    #[error(transparent)]
    Store(#[from] iaam_store::StoreError),
}

/// What an export produced.
///
/// Printed for the operator's own confirmation that the file now on disk is
/// the archive the store just built. It restates the [`Bundle`]'s own
/// metadata rather than reading the file back: a second reading could
/// disagree with the first only by a bug in this function, and a bug here
/// should fail the write, not be reported as a fact about it (§3.4 makes the
/// same choice for the HTTP surface: a value is stated once, by whatever
/// computed it, and never rederived by whoever prints it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub path: String,
    pub bundle_version: u32,
    pub schema_version: u32,
    pub exported_at: String,
    pub owner: OwnerId,
    pub events: usize,
}

impl std::fmt::Display for ExportSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wrote {} (bundle format {}, schema {}, exported {}, {} events)",
            self.path, self.bundle_version, self.schema_version, self.exported_at, self.events
        )
    }
}

/// Export the sole owner's bundle to `path`.
///
/// Refuses when `path` already exists, rather than overwriting a previous
/// archive silently: the file this writes is meant to be kept, and a
/// clobbered backup is discovered only when it is needed and is not there.
pub fn export_to_file(store: &SqliteStore, path: &Path) -> Result<ExportSummary, BundleCliError> {
    let owner = sole_owner(store)?;
    let bundle = store.export_bundle(owner)?;

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                BundleCliError::OutputExists {
                    path: display(path),
                }
            } else {
                BundleCliError::Create {
                    path: display(path),
                    source,
                }
            }
        })?;
    serde_json::to_writer(BufWriter::new(file), &bundle).map_err(|source| {
        BundleCliError::Serialize {
            path: display(path),
            source,
        }
    })?;

    Ok(ExportSummary {
        path: display(path),
        bundle_version: bundle.bundle_version,
        schema_version: bundle.schema_version,
        exported_at: bundle.exported_at,
        owner: bundle.owner,
        events: bundle.events.len(),
    })
}

/// What a restore did.
///
/// `outcome` is [`ImportOutcome`] exactly as `SqliteStore::import_bundle`
/// returned it — see the module doc comment on why this does not attempt a
/// finer per-section account of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub path: String,
    pub bundle_version: u32,
    pub schema_version: u32,
    pub exported_at: String,
    /// The owner the archive was exported under. Differs from
    /// [`Self::restored_owner`] exactly when the archive travelled to a
    /// different instance; see the module doc comment.
    pub archive_owner: OwnerId,
    /// The owner the facts were actually recorded under: this instance's
    /// sole owner, always.
    pub restored_owner: OwnerId,
    pub outcome: ImportOutcome,
    /// Every table the schema declares that a bundle does not carry, and why
    /// — `iaam_store::bundle::TABLE_DISPOSITIONS` filtered to everything but
    /// [`TableDisposition::Carried`]. Always populated, whether or not the
    /// archive being restored is old enough to predate some of these tables:
    /// what a bundle leaves out today is a property of this build, not of
    /// the file just read.
    pub not_carried: Vec<(&'static str, TableDisposition)>,
}

impl std::fmt::Display for RestoreReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "restored {} (bundle format {}, schema {}, exported {})",
            self.path, self.bundle_version, self.schema_version, self.exported_at
        )?;
        if self.archive_owner != self.restored_owner {
            writeln!(
                f,
                "archive was recorded under owner {}; tokens do not travel in a \
                 bundle, so it is attached to this instance's own owner {} instead",
                self.archive_owner.inner(),
                self.restored_owner.inner()
            )?;
        }
        match self.outcome {
            ImportOutcome::Applied {
                inserted,
                duplicates,
            } => writeln!(
                f,
                "events: {inserted} written, {duplicates} already recorded"
            )?,
        }
        if self.not_carried.is_empty() {
            write!(f, "every table in this build's schema travels in a bundle")
        } else {
            writeln!(
                f,
                "not restored — {} table(s) this build's schema holds do not travel in a bundle:",
                self.not_carried.len()
            )?;
            for (index, (table, disposition)) in self.not_carried.iter().enumerate() {
                if index > 0 {
                    writeln!(f)?;
                }
                write!(f, "  {table}: {}", disposition_reason(*disposition))?;
            }
            Ok(())
        }
    }
}

/// Why one table did not travel, in the owner's own words rather than the
/// enum's — printed on [`RestoreReport`] so a table's absence reads as a
/// stated decision instead of something the owner has to notice for himself.
fn disposition_reason(disposition: TableDisposition) -> String {
    match disposition {
        TableDisposition::Carried => {
            unreachable!("Self::not_carried is filtered to exclude Carried")
        }
        TableDisposition::Derived => {
            "recomputed from the journal on restore, not carried".to_owned()
        }
        TableDisposition::Credential => {
            "a credential; never copied into a portable file".to_owned()
        }
        TableDisposition::Pending(bead) => format!("not yet carried; tracked as {bead}"),
    }
}

/// Restore the archive at `path` into `store`.
///
/// Refuses, before the file is even opened, when the instance already holds
/// journal facts for its owner and `merge` was not passed — see the module
/// doc comment for why the journal specifically is the gate. Also refuses
/// when the instance has no owner yet, or has more than one: a restore has
/// to attach the archive's facts to exactly one identity, and both are cases
/// where guessing would be exactly the silent behaviour this exists to
/// prevent.
///
/// **What does not travel here.** `import_bundle`'s own contract already
/// refuses an archive whose format or schema is newer than this build
/// supports, and one whose checksum does not match its contents.
/// [`RestoreReport::not_carried`] names everything else a bundle leaves out
/// — see the module doc comment.
pub fn import_from_file(
    store: &mut SqliteStore,
    path: &Path,
    merge: bool,
) -> Result<RestoreReport, BundleCliError> {
    let owner = sole_owner(store)?;
    if !merge && !store.load_events(owner)?.is_empty() {
        return Err(BundleCliError::NotEmpty);
    }

    let file = File::open(path).map_err(|source| BundleCliError::Open {
        path: display(path),
        source,
    })?;
    let mut bundle: Bundle = serde_json::from_reader(BufReader::new(file)).map_err(|source| {
        BundleCliError::Deserialize {
            path: display(path),
            source,
        }
    })?;

    // Before anything is rewritten: the archive's own two integrity checks,
    // made here on the bytes as they were read. `import_bundle` makes both
    // itself, and on a same-owner restore those are the checks that run — but
    // a restore onto another instance rewrites the owner and the checksum
    // below, and a check made after that rewrite is a check of this command's
    // own arithmetic rather than of the file. See `BundleCliError::Corrupted`.
    if bundle.checksum != bundle.compute_checksum() {
        return Err(BundleCliError::Corrupted {
            path: display(path),
            detail: "checksum does not match contents".to_owned(),
        });
    }
    if let Some(foreign) = bundle
        .events
        .iter()
        .find(|event| event.owner != bundle.owner)
    {
        return Err(BundleCliError::Corrupted {
            path: display(path),
            detail: format!(
                "event {} belongs to another owner than the archive does",
                foreign.id.inner()
            ),
        });
    }

    let archive_owner = bundle.owner;
    if archive_owner != owner {
        bundle.owner = owner;
        for event in &mut bundle.events {
            event.owner = owner;
        }
        bundle.checksum = bundle.compute_checksum();
    }

    let bundle_version = bundle.bundle_version;
    let schema_version = bundle.schema_version;
    let exported_at = bundle.exported_at.clone();
    let outcome = store.import_bundle(&bundle)?;

    let not_carried = TABLE_DISPOSITIONS
        .iter()
        .filter(|(_, disposition)| !matches!(disposition, TableDisposition::Carried))
        .map(|(table, disposition)| (*table, *disposition))
        .collect();

    Ok(RestoreReport {
        path: display(path),
        bundle_version,
        schema_version,
        exported_at,
        archive_owner,
        restored_owner: owner,
        outcome,
        not_carried,
    })
}

/// The instance's one owner, or a refusal naming why there is not exactly
/// one — the same three-way split `iaam token issue` already makes
/// (`main.rs::issue_token`), read here from the store directly because a
/// bundle command has no reason to go through `TokenAdmin` for it.
fn sole_owner(store: &SqliteStore) -> Result<OwnerId, BundleCliError> {
    match store.sole_token_owner()? {
        SoleOwner::None => Err(BundleCliError::Unclaimed),
        SoleOwner::Several => Err(BundleCliError::AmbiguousOwner),
        SoleOwner::Single(owner) => Ok(owner),
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iaam_core::dates::{CashPostedDate, EffectiveOrder, EventDates};
    use iaam_core::event::kind::EventKind;
    use iaam_core::event::leg::Leg;
    use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
    use iaam_core::event::{Confidence, Event, Relation};
    use iaam_core::ids::{AccountId, EventId, SourceId};
    use iaam_core::money::{CurrencyCode, Money, PostedMinor};
    use iaam_core::reconciliation::evidence::IdentityScope;
    use iaam_store::reference::AccountRecord;
    use iaam_store::tokens::{TokenRecord, TokenScope};
    use time::macros::date;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "iaam-bootstrap-bundle-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// A freshly claimed store: one owner, one token, an in-memory database.
    /// `export_to_file` and `import_from_file` both take an already-open
    /// `&SqliteStore`/`&mut SqliteStore`, so nothing here needs a file on
    /// disk except the archive itself.
    fn claimed_store() -> (SqliteStore, OwnerId) {
        let store = SqliteStore::open_in_memory().expect("open store");
        let owner = OwnerId::new_random();
        store
            .insert_token(
                &TokenRecord {
                    id: uuid::Uuid::new_v4(),
                    owner,
                    label: "Main".into(),
                    scope: TokenScope::Owner,
                    revoked: false,
                },
                &format!("hash-{}", uuid::Uuid::new_v4()),
            )
            .expect("insert token");
        (store, owner)
    }

    fn deposit(owner: OwnerId, account: AccountId, sequence: u32, minor: i64) -> Event {
        let amount = Money::new(PostedMinor::new(minor), CurrencyCode::Rub);
        let day = date!(2026 - 05 - 05);
        Event {
            id: EventId::new_random(),
            owner,
            account,
            kind: EventKind::CashIn { amount },
            dates: EventDates::for_cash(CashPostedDate(day)),
            order: EffectiveOrder::new(day, sequence),
            legs: vec![Leg::cash(account, amount)],
            provenance: Provenance::new(
                SourceId::new_random(),
                RawHash::parse(&"7".repeat(64)).unwrap(),
                ParserVersion("manual/1".into()),
            ),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: None,
        }
    }

    fn seed_account(store: &SqliteStore, owner: OwnerId) -> AccountId {
        let account = AccountId::new_random();
        store
            .upsert_account(&AccountRecord {
                id: account,
                owner,
                title: "Main".into(),
                institution: None,
            })
            .expect("seed account");
        account
    }

    #[test]
    fn a_bundle_round_trips_through_a_file() {
        let (mut store, owner) = claimed_store();
        let account = seed_account(&store, owner);
        store
            .append_event(&deposit(owner, account, 1, 10_000), IdentityScope::Source)
            .expect("append event");

        let out_path = temp_path("export");
        let summary = export_to_file(&store, &out_path).expect("export");
        assert_eq!(summary.events, 1);
        assert_eq!(summary.owner, owner);

        let (mut target, target_owner) = claimed_store();
        seed_account(&target, target_owner);
        let report = import_from_file(&mut target, &out_path, false).expect("import");
        assert_eq!(
            report.outcome,
            ImportOutcome::Applied {
                inserted: 1,
                duplicates: 0
            }
        );

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn a_restore_into_an_empty_database_needs_no_merge_flag() {
        let (mut store, owner) = claimed_store();
        let account = seed_account(&store, owner);
        store
            .append_event(&deposit(owner, account, 1, 10_000), IdentityScope::Source)
            .expect("append event");
        let out_path = temp_path("export-empty-target");
        export_to_file(&store, &out_path).expect("export");

        let (mut target, _) = claimed_store();
        let report = import_from_file(&mut target, &out_path, false);
        assert!(report.is_ok(), "empty target should not need --merge");

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn a_restore_into_a_nonempty_database_is_refused_without_merge() {
        let (mut store, owner) = claimed_store();
        let account = seed_account(&store, owner);
        store
            .append_event(&deposit(owner, account, 1, 10_000), IdentityScope::Source)
            .expect("append event");
        let out_path = temp_path("export-for-nonempty");
        export_to_file(&store, &out_path).expect("export");

        let (mut target, target_owner) = claimed_store();
        let target_account = seed_account(&target, target_owner);
        target
            .append_event(
                &deposit(target_owner, target_account, 2, 5_000),
                IdentityScope::Source,
            )
            .expect("append event on target");

        let refusal = import_from_file(&mut target, &out_path, false)
            .expect_err("non-empty target without --merge must be refused");
        assert!(matches!(refusal, BundleCliError::NotEmpty));

        let merged = import_from_file(&mut target, &out_path, true)
            .expect("non-empty target with --merge must succeed");
        assert_eq!(
            merged.outcome,
            ImportOutcome::Applied {
                inserted: 1,
                duplicates: 0
            }
        );

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn a_restore_remaps_the_archives_owner_to_this_instances_own() {
        let (mut store, source_owner) = claimed_store();
        let account = seed_account(&store, source_owner);
        store
            .append_event(
                &deposit(source_owner, account, 1, 10_000),
                IdentityScope::Source,
            )
            .expect("append event");
        let out_path = temp_path("export-for-remap");
        export_to_file(&store, &out_path).expect("export");

        let (mut target, target_owner) = claimed_store();
        assert_ne!(source_owner, target_owner);
        let report = import_from_file(&mut target, &out_path, false).expect("import");
        assert_eq!(report.archive_owner, source_owner);
        assert_eq!(report.restored_owner, target_owner);
        assert_ne!(report.archive_owner, report.restored_owner);

        let restored = target.load_events(target_owner).expect("load events");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].owner, target_owner);

        std::fs::remove_file(&out_path).ok();
    }

    /// A tampered archive is refused **on the machine it was carried to**,
    /// which is the only machine where tampering with it is worth anything.
    ///
    /// The remap that makes a cross-instance restore work rewrites every
    /// event's owner and then the checksum, so a check made after it would be
    /// a check of freshly computed arithmetic and would pass over any altered
    /// amount in the file. This drives the whole path — export on one
    /// instance, edit the file, restore on another — rather than calling the
    /// verification directly, because the ordering is the property under test
    /// and only the whole path exercises it.
    #[test]
    fn a_tampered_archive_is_refused_even_when_the_owner_is_remapped() {
        let (mut store, source_owner) = claimed_store();
        let account = seed_account(&store, source_owner);
        store
            .append_event(
                &deposit(source_owner, account, 1, 10_000),
                IdentityScope::Source,
            )
            .expect("append event");
        let out_path = temp_path("export-for-tamper");
        export_to_file(&store, &out_path).expect("export");

        // A substituted monetary value, which is the failure the checksum
        // exists to catch (§14), left otherwise well-formed so that it is the
        // verification and not the parser that refuses it.
        let text = std::fs::read_to_string(&out_path).expect("read archive");
        let tampered = text.replace("10000", "99999");
        assert_ne!(
            tampered, text,
            "the substitution must actually change the file"
        );
        std::fs::write(&out_path, &tampered).expect("write tampered archive");

        let (mut target, target_owner) = claimed_store();
        assert_ne!(source_owner, target_owner);
        let error =
            import_from_file(&mut target, &out_path, false).expect_err("tampered archive refused");
        assert!(
            matches!(error, BundleCliError::Corrupted { .. }),
            "a tampered archive must be refused before the owner remap recomputes its              checksum, not accepted because of it: {error}"
        );
        assert!(
            target
                .load_events(target_owner)
                .expect("load events")
                .is_empty(),
            "nothing may be written from an archive that failed its own checks"
        );

        std::fs::remove_file(&out_path).ok();
    }

    /// Two archives joined into one is the other thing `import_bundle` refuses
    /// and the remap would otherwise launder: rewriting every event's owner to
    /// this instance's makes a bundle carrying two owners' events uniform, and
    /// therefore acceptable, before the store ever looks at it.
    #[test]
    fn an_archive_carrying_a_second_owners_event_is_refused_before_the_remap() {
        let (mut store, source_owner) = claimed_store();
        let account = seed_account(&store, source_owner);
        store
            .append_event(
                &deposit(source_owner, account, 1, 10_000),
                IdentityScope::Source,
            )
            .expect("append event");
        let out_path = temp_path("export-for-joined");
        export_to_file(&store, &out_path).expect("export");

        let file = File::open(&out_path).expect("open archive");
        let mut bundle: Bundle = serde_json::from_reader(BufReader::new(file)).expect("parse");
        let stranger = OwnerId::new_random();
        bundle.events.push(deposit(stranger, account, 2, 5_000));
        bundle.checksum = bundle.compute_checksum();
        std::fs::write(
            &out_path,
            serde_json::to_string(&bundle).expect("serialise"),
        )
        .expect("write joined archive");

        let (mut target, target_owner) = claimed_store();
        let error =
            import_from_file(&mut target, &out_path, false).expect_err("joined archive refused");
        assert!(
            matches!(error, BundleCliError::Corrupted { .. }),
            "an archive whose events do not all belong to it must be refused, not made              uniform by the remap: {error}"
        );
        assert!(
            target
                .load_events(target_owner)
                .expect("load events")
                .is_empty(),
            "nothing may be written from an archive that failed its own checks"
        );

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn export_refuses_an_unclaimed_instance() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let out_path = temp_path("export-unclaimed");
        let error = export_to_file(&store, &out_path).expect_err("unclaimed export refused");
        assert!(matches!(error, BundleCliError::Unclaimed));
    }

    #[test]
    fn import_refuses_an_unclaimed_instance() {
        let (mut store, owner) = claimed_store();
        let account = seed_account(&store, owner);
        store
            .append_event(&deposit(owner, account, 1, 10_000), IdentityScope::Source)
            .expect("append event");
        let out_path = temp_path("export-for-unclaimed-target");
        export_to_file(&store, &out_path).expect("export");

        let mut target = SqliteStore::open_in_memory().expect("open store");
        let error =
            import_from_file(&mut target, &out_path, false).expect_err("unclaimed target refused");
        assert!(matches!(error, BundleCliError::Unclaimed));

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn a_restore_report_names_every_table_that_did_not_travel() {
        let (mut store, owner) = claimed_store();
        let account = seed_account(&store, owner);
        store
            .append_event(&deposit(owner, account, 1, 10_000), IdentityScope::Source)
            .expect("append event");
        let out_path = temp_path("export-for-not-carried");
        export_to_file(&store, &out_path).expect("export");

        let (mut target, target_owner) = claimed_store();
        seed_account(&target, target_owner);
        let report = import_from_file(&mut target, &out_path, false).expect("import");

        // Tokens are the clearest case: they must never travel, so the
        // report must name them every single time, not just when the
        // archive happened to omit them.
        assert!(
            report
                .not_carried
                .iter()
                .any(|(table, disposition)| *table == "api_tokens"
                    && matches!(disposition, TableDisposition::Credential)),
            "api_tokens must be reported as a credential that did not travel: {:?}",
            report.not_carried
        );
        let rendered = report.to_string();
        assert!(rendered.contains("api_tokens"));
        assert!(rendered.contains("a credential"));

        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn export_refuses_to_overwrite_an_existing_file() {
        let (store, owner) = claimed_store();
        seed_account(&store, owner);
        let out_path = temp_path("export-exists");
        std::fs::write(&out_path, b"not a bundle").expect("seed existing file");

        let error = export_to_file(&store, &out_path).expect_err("existing file refused");
        assert!(matches!(error, BundleCliError::OutputExists { .. }));

        std::fs::remove_file(&out_path).ok();
    }
}
