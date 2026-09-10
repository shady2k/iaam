//! Migrations.
//!
//! Migrations are numbered and applied one at a time in a transaction. The schema file
//! is embedded in the binary: a database opened by this program version must match this version, not whatever is on disk beside it.
//! correspond to this version, rather than what is nearby on disk.

use rusqlite::Connection;

use crate::StoreError;

/// Schema version understood by this build.
///
/// The 31 migrations that built up the schema incrementally collapsed into
/// one file (iaam-05gi): the database was greenfield when this happened —
/// `dev.db` carried no rows in any table — so there was nothing to migrate,
/// and the whole schema starts fresh at version 1.
///
/// **The collapse was a one-off and version 2 is where ordinary numbering
/// resumes** (iaam-k3gh.8). A database holding rows cannot have a column added
/// to `0001_schema.sql`, because it is already at version 1 and skips that file
/// altogether; the column would then exist in every database created after the
/// change and in none created before it, and the failure would arrive at run
/// time, on a statement naming a column that is not there.
pub const SCHEMA_VERSION: u32 = 5;

/// Which numbering [`SCHEMA_VERSION`] belongs to.
///
/// `SCHEMA_VERSION` counts migrations *within* a numbering scheme, and that
/// scheme has already been reset once — the collapse this module's own doc
/// comment describes. A version number alone cannot tell "migration 2 of the
/// scheme this build implements" from "migration 2 of the scheme the collapse
/// discarded": both are the bare integer `2`, and an archive stamped with the
/// wrong one is not an old copy of this schema, it is a description of a
/// schema that no longer exists (iaam-k3gh.9.5).
///
/// Bumped only when [`SCHEMA_VERSION`]'s own counter is reset, never on an
/// ordinary migration — exactly the discipline `SCHEMA_VERSION` already
/// documents for itself. Nothing has forced that since the collapse, so this
/// is generation one: the first (and, so far, only) numbering this constant
/// has ever named. A bundle archive (`iaam_store::bundle::Bundle`) carries
/// this beside its own `schema_version` so `import_bundle` can refuse a
/// mismatch by name rather than comparing two numbers that happen to collide.
pub const SCHEMA_GENERATION: u32 = 1;

/// The highest `schema_version` any build ever wrote **without** stamping
/// [`SCHEMA_GENERATION`] beside it.
///
/// The marker was added after the collapse, so an archive that carries none
/// is not thereby of this generation — it is an archive from before anyone
/// wrote the generation down, and the counter it used cannot be recovered
/// from the file. What can be recovered is this: every build that wrote no
/// marker had `SCHEMA_VERSION` at this value or below, because the marker
/// landed with the migration that raised it. So an unmarked archive claiming
/// more than this was written under the numbering the collapse discarded —
/// which is exactly the archive iaam-k3gh.9.5 is about, and exactly the one
/// that would otherwise be waved through by reading its absent marker as
/// agreement.
///
/// The one archive this refuses that is not from the discarded numbering is
/// one exported in the short window between the migration that raised
/// `SCHEMA_VERSION` to 2 and the marker landing hours later. Re-exporting it
/// costs a command; accepting an archive whose numbering nobody can name
/// costs a journal.
pub const LAST_UNMARKED_SCHEMA_VERSION: u32 = 1;

/// Every migration, numbered and embedded, in application order.
///
/// `pub` so `tests/bundle_coverage.rs` can read the whole schema without
/// hard-coding a second list of migration filenames: that test's job is to
/// notice a `CREATE TABLE` this crate forgot to classify, and it can only do
/// that against the same migrations this build actually applies.
pub const MIGRATIONS: [(u32, &str); 5] = [
    (1, include_str!("../migrations/0001_schema.sql")),
    (
        2,
        include_str!("../migrations/0002_source_counterparty.sql"),
    ),
    (
        3,
        include_str!("../migrations/0003_event_stated_securities_value.sql"),
    ),
    (
        4,
        include_str!("../migrations/0004_account_declared_by.sql"),
    ),
    (
        5,
        include_str!("../migrations/0005_account_retractions.sql"),
    ),
];

/// Apply missing migrations.
///
/// The database is newer than the program—reject it rather than attempt to operate: an unknown
/// column is silently read as absent, which is the worst kind of error.
pub fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        if version <= current {
            continue;
        }
        apply(conn, version, sql)?;
    }
    Ok(())
}

/// Apply one migration, unless another connection got there first.
///
/// The version read at the top of `migrate` is only a hint: between that read and this
/// call another process—the server starting, a CLI command run a moment later—may have
/// applied the very same migration. So the transaction opens with `BEGIN IMMEDIATE`,
/// taking the write lock at once rather than at the first write, and the version is read
/// again inside it. The loser of the race then sees the work already done and commits
/// nothing, instead of failing halfway through with `table … already exists`.
///
/// **`foreign_keys` is off for the duration, on every migration.** SQLite has
/// no `ALTER TABLE … ADD CONSTRAINT`: widening the `CHECK` that names
/// `events.kind`'s vocabulary — which `0003_event_stated_securities_value.sql`
/// does — means rebuilding that table, and `DROP TABLE events` is refused
/// outright while the fifteen tables that hold a foreign key to it are
/// enforced, on rows that are not moving and not becoming invalid. SQLite
/// only allows the pragma to change with no transaction pending, so it has
/// to happen here, around `BEGIN IMMEDIATE`, and not inside a migration's own
/// SQL, where it would already be too late to take effect. Applied to every
/// migration rather than singled out for the one that currently needs it: a
/// later migration that rebuilds a table is the ordinary case a schema this
/// size will keep meeting, not an exception one flag should have to name.
fn apply(conn: &Connection, version: u32, sql: &str) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let result = apply_inner(conn, version, sql);
    // Restored unconditionally, success or failure: a migration that failed
    // must not leave later, unrelated writes on this connection running with
    // the guard off.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    result
}

fn apply_inner(conn: &Connection, version: u32, sql: &str) -> Result<(), StoreError> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let outcome = apply_inside_transaction(conn, version, sql);
    match outcome {
        Ok(true) => conn.execute_batch("COMMIT;")?,
        Ok(false) | Err(_) => {
            // Rolling back a transaction SQLite has already unwound itself is harmless,
            // and leaving one open would poison every later statement on this connection.
            let _ = conn.execute_batch("ROLLBACK;");
            outcome?;
        }
    }
    Ok(())
}

/// Runs the migration and reports whether anything was written.
fn apply_inside_transaction(
    conn: &Connection,
    version: u32,
    sql: &str,
) -> Result<bool, StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current >= version {
        return Ok(false);
    }
    conn.execute_batch(&format!("{sql} PRAGMA user_version = {version};"))?;
    refuse_broken_references(conn, version)?;
    Ok(true)
}

/// Refuse a migration that left a child row pointing at a parent that is not
/// there, while the transaction can still be rolled back.
///
/// Every migration here runs with `PRAGMA foreign_keys` off, because a
/// migration that rebuilds a table drops the parent the fifteen detail and leg
/// tables reference and would otherwise be refused outright — see [`apply`].
/// The cost of switching the guard off is that nothing checks it, and the one
/// migration that has needed the rebuild is the one that copies every row of
/// the owner's journal from one table into another: exactly the operation
/// whose failure mode is a row left behind.
///
/// SQLite's own procedure for a table rebuild ends with this check for that
/// reason, and it is run for every migration rather than for the rebuild
/// alone: a migration that does not touch a foreign key passes it in
/// microseconds, and the next one that does need it will not have to remember
/// to ask.
///
/// Inside the transaction, deliberately. Reporting the damage after the commit
/// would be a description of the owner's broken journal rather than a refusal
/// to break it.
fn refuse_broken_references(conn: &Connection, version: u32) -> Result<(), StoreError> {
    let violations: i64 = conn
        .prepare("SELECT count(*) FROM pragma_foreign_key_check")?
        .query_row([], |row| row.get(0))?;
    if violations > 0 {
        return Err(StoreError::MigrationBrokeReferences {
            version,
            violations: violations.unsigned_abs() as usize,
        });
    }
    Ok(())
}
