//! Two processes opening the same database file at once.
//!
//! The server starting and a CLI command run immediately after are two writers
//! racing on a fresh file. Both read `user_version = 0`, both decide the same
//! migration is pending, and the loser used to fail with `table events already
//! exists`. Threads stand in for the processes: they share nothing but the file,
//! which is exactly what the two processes share.

use std::fs;
use std::path::PathBuf;
use std::sync::Barrier;
use std::thread;

use iaam_store::SqliteStore;
use iaam_store::schema::SCHEMA_VERSION;
use uuid::Uuid;

/// Directory for the file-based database. The file is needed literally:
/// `open_in_memory` gives each connection a private database, so two of them
/// never contend for the same lock.
struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    fn create(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("iaam-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("database directory");
        Self { path }
    }

    fn file(&self) -> PathBuf {
        self.path.join("iaam.sqlite3")
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn two_connections_migrating_the_same_file_at_once_both_succeed() {
    let directory = TempDatabase::create("concurrent-migration");
    let file = directory.file();
    // The barrier makes the race the point of the test rather than a matter of
    // thread start-up luck: neither connection opens the file until both are ready.
    let gate = Barrier::new(2);

    thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|index| {
                let file = file.clone();
                let gate = &gate;
                scope.spawn(move || {
                    gate.wait();
                    SqliteStore::open(&file)
                        .unwrap_or_else(|error| panic!("connection {index} migrates: {error}"));
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("migrating thread does not panic");
        }
    });

    let store = SqliteStore::open(&file).expect("reopening the migrated database");
    let version: u32 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("reading the schema version");
    assert_eq!(
        version, SCHEMA_VERSION,
        "the race leaves the schema fully migrated exactly once"
    );
}
