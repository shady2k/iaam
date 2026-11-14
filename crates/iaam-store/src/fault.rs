//! Named stop points for the kill harness (ADR 0042 point 7).
//!
//! A test arms one point and runs its work in a child process; the child
//! touches a marker and waits at the point, the parent kills it, and reopens
//! the database. What the reopened process finds is exactly what a kill at
//! that instant leaves.
//!
//! The shape is the one `iaam_http::test_support::arm_tally_barrier` uses: one
//! armed point per process, a marker file, and a sleep that never returns.
//! A process that arms nothing passes every point. This module exists only
//! with `cfg(test)` or the `test-support` feature, and that feature is
//! refused in release builds by a `compile_error!` in the crate root.
//!
//! **The work is a test function of the same binary.** A closure cannot cross
//! a process boundary, so [`kill_at`] re-executes the test executable with
//! `--exact <child test>`; the child test calls [`arm_child_barrier`] at its
//! top and is the closure's body. Everything the child needs travels in
//! environment variables named by the constants below.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::SqliteStore;

/// Reached after `after_write` writes of a `Store::transact` work, before the
/// transaction commits.
pub const BEFORE_COMMIT: &str = "before-commit";
/// Reached after the transaction committed and before `Store::transact`
/// returns.
pub const AFTER_COMMIT: &str = "after-commit";
/// Reached inside one migration's transaction, after its SQL and its
/// foreign-key check, before it commits.
pub const MIGRATION_INSIDE_TRANSACTION: &str = "migration-inside-transaction";

/// The point the child arms.
pub const BARRIER_VARIABLE: &str = "IAAM_FAULT_BARRIER";
/// The marker file the child touches when it reaches the point.
pub const MARKER_VARIABLE: &str = "IAAM_FAULT_MARKER";
/// How many writes must complete before the point is reached; empty means
/// «on reaching the point itself».
pub const AFTER_WRITES_VARIABLE: &str = "IAAM_FAULT_AFTER_WRITES";
/// The database the child opens.
pub const DATABASE_VARIABLE: &str = "IAAM_FAULT_DATABASE";

struct Armed {
    point: String,
    marker: PathBuf,
    after_write: Option<usize>,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);
static WRITES: AtomicUsize = AtomicUsize::new(0);

/// Arms `point`: reaching it touches `marker` and waits to be killed.
///
/// When `after_write` is `Some(n)`, the point is reached once `n` writes of
/// the current transaction have completed; when it is `None`, reaching the
/// point itself is enough. Only the last armed point is honoured.
pub fn arm_barrier(point: &str, marker: &Path, after_write: Option<usize>) {
    *ARMED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Armed {
        point: point.to_owned(),
        marker: marker.to_owned(),
        after_write,
    });
}

/// Child side: arms the point named by the parent's environment.
///
/// Returns `false`, arming nothing, when this process is not a fault child.
/// A child test calls this first and returns on `false`, so the same test
/// function is harmless in an ordinary test run.
pub fn arm_child_barrier() -> bool {
    let Some(point) = std::env::var_os(BARRIER_VARIABLE) else {
        return false;
    };
    let marker = std::env::var_os(MARKER_VARIABLE).expect("fault child marker configured");
    let after_write = std::env::var(AFTER_WRITES_VARIABLE)
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .expect("fault child write threshold is a number")
        });
    arm_barrier(&point.to_string_lossy(), Path::new(&marker), after_write);
    true
}

/// Starts one transaction: the write count is per transaction, so a batch is
/// stopped after its own first write and not after some earlier one.
pub(crate) fn transaction_began() {
    WRITES.store(0, Ordering::SeqCst);
}

/// One write of the current transaction completed.
pub(crate) fn write_completed() {
    let writes = WRITES.fetch_add(1, Ordering::SeqCst) + 1;
    stop_if_armed(BEFORE_COMMIT, Some(writes));
}

/// Reached a named point of the transaction machinery.
pub(crate) fn transaction_point(point: &str) {
    stop_if_armed(point, None);
}

/// Reached a named point inside a migration's transaction.
pub(crate) fn migration_point(point: &str) {
    stop_if_armed(point, None);
}

fn stop_if_armed(point: &str, writes: Option<usize>) {
    let marker = {
        let armed = ARMED.lock().unwrap_or_else(PoisonError::into_inner);
        match armed.as_ref() {
            Some(armed)
                if armed.point == point
                    && writes.is_none_or(|writes| armed.after_write == Some(writes)) =>
            {
                armed.marker.clone()
            }
            _ => return,
        }
    };
    std::fs::write(&marker, point.as_bytes()).expect("the fault barrier marker is written");
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

/// A child that reached its barrier and was killed by [`kill_at`].
pub struct KilledChild {
    /// The marker the child touched at the barrier.
    pub marker: PathBuf,
    /// How the child ended.
    pub status: ExitStatus,
}

/// The store reopened after the kill, beside the child that was killed.
pub struct KillAt {
    /// The database reopened by the parent.
    pub store: SqliteStore,
    /// How the child ended and where it stopped.
    pub child: KilledChild,
}

/// Runs `child_test` in a child process at the armed barrier, kills it there,
/// and reopens `database`.
///
/// `child_test` names a test function in the calling test binary; it is the
/// work of the closure this helper stands for, selected with `--exact` because
/// a closure itself cannot cross a process boundary. `point` and `after_write`
/// are armed for it; `variables` are extra environment variables the child
/// test needs (fixture identifiers, usually).
///
/// The child is SIGKILLed, so it gets no chance to clean up: what the reopened
/// store holds is exactly what survives that instant. The reopen runs the
/// store's ordinary `open_existing`, which is what the server's next start
/// would do.
pub fn kill_at(
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    marker: &Path,
    database: &Path,
    variables: &[(&str, &OsStr)],
) -> KillAt {
    let child = kill_child_at(child_test, point, after_write, marker, database, variables);
    KillAt {
        store: SqliteStore::open_existing(database).expect("the killed database reopens"),
        child,
    }
}

/// The process half of [`kill_at`]: kill at the barrier, without reopening.
///
/// A caller that must inspect the file before the store's own migrations run
/// — the migration kill test — uses this and opens the file itself.
pub fn kill_child_at(
    child_test: &str,
    point: &str,
    after_write: Option<usize>,
    marker: &Path,
    database: &Path,
    variables: &[(&str, &OsStr)],
) -> KilledChild {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", child_test, "--nocapture"])
        .env(BARRIER_VARIABLE, point)
        .env(MARKER_VARIABLE, marker)
        .env(
            AFTER_WRITES_VARIABLE,
            after_write.map_or_else(String::new, |value| value.to_string()),
        )
        .env(DATABASE_VARIABLE, database)
        .stdout(Stdio::null());
    for (name, value) in variables {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("fault child started");
    wait_for_marker(&mut child, marker);
    child.kill().expect("fault child killed");
    let status = child.wait().expect("fault child reaped");
    KilledChild {
        marker: marker.to_owned(),
        status,
    }
}

fn wait_for_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the child did not reach the fault barrier"
        );
        assert_eq!(
            child.try_wait().expect("child status"),
            None,
            "the child exited before the fault barrier"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
