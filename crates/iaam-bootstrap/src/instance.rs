//! The instance's places on disk.
//!
//! One rule guards everything here: **only `iaam claim` creates a
//! database.** The resolver in `config` says where the instance's files
//! live; this module creates what the creating commands create, and nothing
//! else. An empty database or an empty key appearing silently under a
//! default path is indistinguishable from a lost portfolio, so no command
//! but the two named ones makes a file or a directory at these places.

use std::os::unix::fs::PermissionsExt;

use std::fs;
use std::io;
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstanceError {
    #[error("cannot create the directory {path} for {what}: {source}")]
    Directory {
        path: String,
        what: &'static str,
        #[source]
        source: io::Error,
    },
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
    use super::ensure_private_directory;

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
}
