use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const BROKER_EGRESS_ENV: &str = "IAAM_BROKER_EGRESS";

/// Whether this process may send requests to a broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerEgress {
    /// Broker destinations are refused before the tally or transport.
    Off,
    /// Broker destinations use the tally directory derived from the
    /// instance's database path.
    On,
}

/// Invalid broker-egress process configuration.
#[derive(Debug, Error)]
pub enum BrokerEgressConfigError {
    #[error("{BROKER_EGRESS_ENV} has invalid value {value:?}; use `on` or `off`")]
    InvalidSwitch { value: String },
    #[error("{BROKER_EGRESS_ENV} is not valid Unicode; use `on` or `off`")]
    NonUnicodeSwitch,
}

impl BrokerEgress {
    /// Read the process's broker-egress switch alone: unset means off.
    ///
    /// Commands that do not read the instance's database — and every caller
    /// from before the switch was stored in it — ask this.
    ///
    /// # Errors
    /// An invalid switch value or a non-Unicode switch.
    pub fn from_env() -> Result<Self, BrokerEgressConfigError> {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// The effective switch: the environment variable over the instance's
    /// stored one (migration `0008`).
    ///
    /// Unset, the stored word stands: a successful `iaam broker connect`
    /// turned the switch on, and `serve` reads it at start. `off` still forces
    /// broker requests off — a deployment that must not call a broker wins
    /// over the stored word — and `on` stays accepted as an override for
    /// developer tools run against an instance whose switch is off.
    ///
    /// # Errors
    /// As [`Self::from_env`].
    pub fn resolve<F>(get: F, stored: bool) -> Result<Self, BrokerEgressConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        match get(BROKER_EGRESS_ENV) {
            None => Ok(if stored { Self::On } else { Self::Off }),
            Some(value) => Self::parse(value),
        }
    }

    fn parse(value: OsString) -> Result<Self, BrokerEgressConfigError> {
        let value = value
            .into_string()
            .map_err(|_| BrokerEgressConfigError::NonUnicodeSwitch)?;
        match value.as_str() {
            "off" => Ok(Self::Off),
            "on" => Ok(Self::On),
            _ => Err(BrokerEgressConfigError::InvalidSwitch { value }),
        }
    }

    fn from_lookup<F>(get: F) -> Result<Self, BrokerEgressConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        Self::resolve(get, false)
    }
}

/// The instance's database could not be resolved to place its egress directory.
#[derive(Debug, Error)]
pub enum EgressDirectoryError {
    /// The database does not exist (or a path part is not a directory), so no
    /// directory can be placed beside it.
    #[error(
        "the database {database} does not exist, so its egress directory cannot live beside it; \
         the gateway is built after the store exists, so check which database this process was given",
        database = .database.display()
    )]
    DatabaseUnresolved {
        database: PathBuf,
        source: std::io::Error,
    },
    /// The resolved database path has no file name to name the directory after.
    #[error(
        "the database path {database} has no file name, so its egress directory cannot be named after it",
        database = .database.display()
    )]
    NoFileName { database: PathBuf },
    /// The database file has more than one name (hard links). Each name would
    /// derive its own egress directory, so two processes opening the same
    /// database by two names would keep two tallies — and one could send
    /// while the other holds a broker's pause.
    #[error(
        "the database {database} has {links} names (hard links); each name would get its own broker tally, \
         so iaam refuses it: keep one name and remove the others (`find -samefile` lists them)",
        database = .database.display()
    )]
    HardLinked { database: PathBuf, links: u64 },
}

/// Where one instance's broker tally lives: a directory beside its database,
/// named after the database file (`iaam.sqlite` → `iaam.sqlite.egress`).
///
/// One database is one tally. Every alias of the same database — a relative
/// path, a redundant `..`, a symlink to the file or to its directory — yields
/// the same directory, because the database path is canonicalized first; a
/// database with a second hard-linked name is refused, since canonicalizing
/// cannot join two names of one file. Two
/// different databases yield two directories: two instances with two databases
/// have two tallies.
///
/// # Errors
/// The database path does not resolve to an existing file
/// ([`EgressDirectoryError::DatabaseUnresolved`]) or has no file name
/// ([`EgressDirectoryError::NoFileName`]).
pub fn egress_directory_for(database: &Path) -> Result<PathBuf, EgressDirectoryError> {
    database_directory_for(database, ".egress")
}

/// The directory named after the database file with `suffix`, derived the
/// way [`egress_directory_for`] derives the egress place: canonicalize the
/// database path so every alias lands on one directory, refuse a
/// hard-linked database (two names would keep two places), and name the
/// directory after the file. The response cache derives its
/// `<database>.cache` place through this too.
///
/// # Errors
/// The same three as [`egress_directory_for`].
pub(crate) fn database_directory_for(
    database: &Path,
    suffix: &str,
) -> Result<PathBuf, EgressDirectoryError> {
    let resolved =
        database
            .canonicalize()
            .map_err(|source| EgressDirectoryError::DatabaseUnresolved {
                database: database.to_owned(),
                source,
            })?;
    let links = std::fs::metadata(&resolved)
        .map_err(|source| EgressDirectoryError::DatabaseUnresolved {
            database: database.to_owned(),
            source,
        })?
        .nlink();
    if links > 1 {
        return Err(EgressDirectoryError::HardLinked {
            database: resolved,
            links,
        });
    }
    let mut name = resolved
        .file_name()
        .ok_or_else(|| EgressDirectoryError::NoFileName {
            database: database.to_owned(),
        })?
        .to_owned();
    name.push(suffix);
    let mut directory = resolved;
    directory.pop();
    Ok(directory.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn broker_egress_defaults_to_off() {
        assert_eq!(
            BrokerEgress::from_lookup(values(&[])).unwrap(),
            BrokerEgress::Off
        );
    }

    #[test]
    fn broker_egress_accepts_explicit_off_without_a_tally() {
        assert_eq!(
            BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "off")])).unwrap(),
            BrokerEgress::Off
        );
    }

    #[test]
    fn broker_egress_accepts_explicit_on_without_another_setting() {
        let configured = BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "on")])).unwrap();

        assert_eq!(configured, BrokerEgress::On);
    }

    #[test]
    fn invalid_switch_names_the_value_and_allowed_values() {
        let error = BrokerEgress::from_lookup(values(&[(BROKER_EGRESS_ENV, "yes")])).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(BROKER_EGRESS_ENV), "{message}");
        assert!(message.contains("yes"), "{message}");
        assert!(message.contains("on"), "{message}");
        assert!(message.contains("off"), "{message}");
    }
}

#[cfg(test)]
mod derivation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn unique_directory(label: &str) -> PathBuf {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "iaam-http-egress-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("test directory created");
        directory
    }

    #[test]
    fn the_directory_is_named_after_the_database_file_beside_it() {
        let directory = unique_directory("named-after");
        let database = directory.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");

        let derived = egress_directory_for(&database).expect("database resolves");

        assert_eq!(derived, directory.join("iaam.sqlite.egress"));
    }

    #[test]
    fn a_missing_database_is_refused_by_name() {
        let directory = unique_directory("missing");
        let database = directory.join("absent.sqlite");

        let error = egress_directory_for(&database).expect_err("missing database refused");

        assert!(error.to_string().contains("absent.sqlite"), "{error}");
    }

    #[test]
    fn a_hard_linked_database_is_refused_by_both_names() {
        let directory = unique_directory("hard-link");
        let database = directory.join("iaam.sqlite");
        let second_name = directory.join("alias.sqlite");
        std::fs::write(&database, "").unwrap();
        std::fs::hard_link(&database, &second_name).unwrap();

        for name in [&database, &second_name] {
            let refused = egress_directory_for(name);
            let Err(error) = refused else {
                panic!("a hard-linked database must be refused, got {refused:?}");
            };
            let text = error.to_string();
            assert!(text.contains("2 names"), "{text}");
            assert!(
                text.contains("iaam.sqlite") || text.contains("alias.sqlite"),
                "{text}"
            );
        }
    }

    #[test]
    fn every_alias_of_one_database_yields_one_directory() {
        let directory = unique_directory("aliases");
        let real = directory.join("real");
        std::fs::create_dir(&real).expect("real directory created");
        let database = real.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");

        let direct = egress_directory_for(&database).expect("direct path resolves");

        // A symlink to the database file.
        let file_link = directory.join("file-link.sqlite");
        std::os::unix::fs::symlink(&database, &file_link).expect("database symlink created");
        // A symlink to the database's directory.
        let directory_link = directory.join("directory-link");
        std::os::unix::fs::symlink(&real, &directory_link).expect("directory symlink created");
        // A path carrying a redundant `..`.
        let dotdot = directory
            .join("directory-link")
            .join("..")
            .join("real")
            .join("iaam.sqlite");

        assert_eq!(
            egress_directory_for(&file_link).expect("file symlink resolves"),
            direct,
            "a symlink to the file must reach the same tally"
        );
        assert_eq!(
            egress_directory_for(&directory_link.join("iaam.sqlite"))
                .expect("directory symlink resolves"),
            direct,
            "a symlink to the directory must reach the same tally"
        );
        assert_eq!(
            egress_directory_for(&dotdot).expect("dot-dot path resolves"),
            direct,
            "a redundant `..` must reach the same tally"
        );
    }

    #[test]
    fn a_relative_path_yields_the_same_directory_as_the_absolute_one() {
        let directory = unique_directory("relative");
        let database = directory.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database file written");
        // A path relative to the current directory, without changing it:
        // walk from the working directory down to the common ancestor, then
        // back up to the database.
        let current = std::env::current_dir().expect("working directory");
        let mut relative = PathBuf::new();
        let mut up = current.as_path();
        while !database.starts_with(up) {
            up = up.parent().expect("common ancestor");
            relative.push("..");
        }
        relative.push(database.strip_prefix(up).expect("below the ancestor"));

        let absolute = egress_directory_for(&database).expect("absolute path resolves");

        assert_eq!(
            egress_directory_for(&relative).expect("relative path resolves"),
            absolute,
        );
    }

    #[test]
    fn two_databases_yield_two_directories() {
        let directory = unique_directory("two");
        let first = directory.join("first.sqlite");
        let second = directory.join("second.sqlite");
        std::fs::write(&first, "").expect("first database written");
        std::fs::write(&second, "").expect("second database written");

        let first_directory = egress_directory_for(&first).expect("first resolves");
        let second_directory = egress_directory_for(&second).expect("second resolves");

        assert_ne!(first_directory, second_directory);
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn lookup_off(_name: &str) -> Option<OsString> {
        Some(OsString::from("off"))
    }

    #[test]
    fn an_unset_variable_lets_the_stored_word_stand() {
        assert_eq!(
            BrokerEgress::resolve(|_| None, true).ok(),
            Some(BrokerEgress::On)
        );
        assert_eq!(
            BrokerEgress::resolve(|_| None, false).ok(),
            Some(BrokerEgress::Off)
        );
    }

    #[test]
    fn an_explicit_off_wins_over_a_stored_on() {
        assert_eq!(
            BrokerEgress::resolve(lookup_off, true).ok(),
            Some(BrokerEgress::Off),
            "a deployment that must not call a broker wins over the stored word"
        );
    }

    #[test]
    fn an_explicit_on_overrides_a_stored_off_for_developer_tools() {
        assert_eq!(
            BrokerEgress::resolve(|_| Some(OsString::from("on")), false).ok(),
            Some(BrokerEgress::On)
        );
    }

    #[test]
    fn an_invalid_value_is_refused_whatever_is_stored() {
        let error = BrokerEgress::resolve(|_| Some(OsString::from("maybe")), true)
            .expect_err("an unknown word is refused");
        assert!(error.to_string().contains("use `on` or `off`"), "{error}");
    }
}
