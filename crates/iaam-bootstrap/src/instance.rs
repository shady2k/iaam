//! The instance's places on disk.
//!
//! One rule guards everything here: **only `iaam claim` creates a
//! database.** The resolver in `config` says where the instance's files
//! live; this module creates what the creating commands create, opens what
//! every other command opens, and nothing else. An empty database or an
//! empty key appearing silently under a default path is indistinguishable
//! from a lost portfolio, so no command but the two named ones makes a file
//! or a directory at these places.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use iaam_store::{SqliteStore, StoreError};
use thiserror::Error;

use crate::config::Place;

#[derive(Debug, Error)]
pub enum InstanceError {
    #[error("cannot create the directory {path} for {what}: {source}")]
    Directory {
        path: String,
        what: &'static str,
        #[source]
        source: io::Error,
    },
    /// No database exists where the command resolved it. The command-facing
    /// refusal lives here, beside the commands that name `iaam claim`; the
    /// store only says that the place is empty.
    #[error(
        "no database at {path}: a database is created only by \
         `iaam claim`, no other command creates one"
    )]
    DatabaseMissing { path: String },
    #[error("cannot open the database {path}: {source}")]
    DatabaseUnreadable {
        path: String,
        #[source]
        source: StoreError,
    },
}

/// Opens the instance's database for every command that must not create it.
///
/// This is the one open path of the non-claiming commands — `serve`, the
/// token, broker and bundle commands, and the existence check of `broker
/// key generate` all go through it — so a missing database is refused with
/// one message naming the place and the command that creates, and no
/// branch can reach a create-if-missing open by accident.
pub fn open_database(place: &Place) -> Result<SqliteStore, InstanceError> {
    SqliteStore::open_existing(&place.path).map_err(|source| match source {
        StoreError::DatabaseMissing { path } => InstanceError::DatabaseMissing { path },
        source => InstanceError::DatabaseUnreadable {
            path: place.path.display().to_string(),
            source,
        },
    })
}

/// Creates the directory that holds the file `of`, mode `0700`, when it is
/// missing.
///
/// Only the two creating commands call this: `iaam claim` for the
/// database's directory, `iaam broker key generate` for the key's. A
/// directory that already exists is left exactly as its owner made it —
/// tightening the mode of a directory somebody else provisioned is not
/// this command's decision. A file with no directory of its own (a bare
/// relative name) sits in the current directory, which exists.
pub fn ensure_private_directory(of: &Path, what: &'static str) -> Result<(), InstanceError> {
    let directory = of.parent().unwrap_or_else(|| Path::new(""));
    if directory.as_os_str().is_empty() || directory.exists() {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|source| InstanceError::Directory {
        path: directory.display().to_string(),
        what,
        source,
    })?;
    // The umask may have removed permission bits during creation: confirm
    // the mode explicitly rather than assuming it.
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
        InstanceError::Directory {
            path: directory.display().to_string(),
            what,
            source,
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_private_directory, open_database};
    use crate::config::{Place, PlaceSource};

    #[test]
    fn a_created_directory_is_private_and_an_existing_one_is_untouched() {
        let base =
            std::env::temp_dir().join(format!("iaam-bootstrap-instance-{}", uuid::Uuid::new_v4()));
        let database = base.join("iaam/iaam.db");
        ensure_private_directory(&database, "test place").expect("directory created");

        use std::os::unix::fs::PermissionsExt;
        let directory = base.join("iaam");
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a created place is private");
        assert!(!database.exists(), "only the directory was created");

        // An existing directory is not re-permissioned: the owner's choice
        // stands.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o750)).unwrap();
        ensure_private_directory(&database, "test place").expect("existing directory accepted");
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750, "an existing directory keeps its mode");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_bare_relative_name_needs_no_directory() {
        ensure_private_directory(std::path::Path::new("iaam.db"), "test place")
            .expect("the current directory exists");
    }

    #[test]
    fn a_missing_database_refuses_naming_the_place_and_the_creating_command() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-open-database-{}",
            uuid::Uuid::new_v4()
        ));
        let place = Place {
            path: path.clone(),
            source: PlaceSource::Default,
        };

        let error = match open_database(&place) {
            Ok(_) => panic!("a missing database is refused, never created"),
            Err(error) => error,
        };

        let text = error.to_string();
        assert!(text.contains("no database at"), "{text}");
        assert!(text.contains(path.display().to_string().as_str()), "{text}");
        assert!(text.contains("iaam claim"), "{text}");
        assert!(!path.exists(), "the refused open must not create the file");
    }

    #[test]
    fn a_file_that_is_no_database_is_refused_as_unreadable() {
        let path = std::env::temp_dir().join(format!(
            "iaam-bootstrap-open-database-garbage-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"this is not a database").unwrap();
        let place = Place {
            path: path.clone(),
            source: PlaceSource::Default,
        };

        let error = match open_database(&place) {
            Ok(_) => panic!("garbage is not accepted as a database"),
            Err(error) => error,
        };

        let text = error.to_string();
        assert!(text.contains("cannot open the database"), "{text}");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_place_that_cannot_be_created_is_refused_with_its_cause() {
        let base = std::env::temp_dir().join(format!(
            "iaam-bootstrap-instance-refused-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        // A dangling symlink in the directory's place: it does not exist
        // (`exists` follows it), and creating through it fails.
        let link = base.join("iaam");
        std::os::unix::fs::symlink(base.join("nowhere"), &link).unwrap();
        let database = link.join("iaam.db");

        let error = match ensure_private_directory(&database, "test place") {
            Ok(()) => panic!("a place behind a dangling symlink is not creatable"),
            Err(error) => error,
        };

        let text = error.to_string();
        assert!(text.contains("cannot create the directory"), "{text}");
        assert!(text.contains("test place"), "{text}");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
