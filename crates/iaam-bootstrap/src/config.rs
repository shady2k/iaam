//! Configuration from the environment.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone, Eq, PartialEq, Error)]
pub enum ConfigError {
    /// Neither the override variable nor a home directory the default place
    /// could hang off is set. The message names every variable that would
    /// supply the place, because the one reading it is the person who must
    /// set one of them.
    #[error(
        "no place for {what}: set {variable}, or {xdg_variable}, or HOME; \
         none of them is set"
    )]
    NoPlace {
        what: &'static str,
        variable: &'static str,
        xdg_variable: &'static str,
    },
    #[error("variable {name} is invalid: {value}; allowed values: {allowed}")]
    Invalid {
        name: &'static str,
        value: String,
        allowed: &'static str,
    },
}

/// Where one file of the instance lives, and who chose the place.
///
/// `iaam status` prints both halves: the path answers "where", the source
/// answers "why there" — a variable was set, or this is the default place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: PathBuf,
    pub source: PlaceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceSource {
    /// Named by an environment variable (`IAAM_DATABASE` or
    /// `IAAM_BROKER_KEY_FILE`).
    Variable,
    /// The instance's default place, under XDG or the home directory.
    Default,
}

/// The places of one instance, and who chose each.
///
/// Everything that touches only the places — `iaam status`, and the key
/// command's existence check — resolves this and nothing else, so an
/// unrelated serve setting cannot stand between the owner and the answer
/// to "where is my instance".
///
/// The database place always resolves: every command needs it, and without
/// it there is no instance to talk about. The key place need not — a
/// container that names no home directory and no key file has none — so it
/// arrives as a result: the commands that need the key read it through
/// [`Places::key_place`], `iaam status` prints the reason, and the
/// commands that never touch the key never ask for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    pub database: Place,
    pub broker_key: Result<Place, ConfigError>,
}

/// The inputs that decide one place: the variable that overrides it, the
/// XDG variable that moves the default, the subpath under each, what the
/// place holds and what a value must look like — the last two so the
/// refusals can name the variables that would supply the place and say
/// what was wrong with the value that was supplied instead.
struct PlaceSpec {
    variable: &'static str,
    xdg_variable: &'static str,
    what: &'static str,
    /// Subpath under the XDG directory, `$XDG_DATA_HOME/iaam/iaam.db`.
    xdg_tail: &'static str,
    /// Subpath under the home directory, `$HOME/.local/share/iaam/iaam.db`.
    home_tail: &'static str,
    allowed: &'static str,
}

const DATABASE_PLACE: PlaceSpec = PlaceSpec {
    variable: "IAAM_DATABASE",
    xdg_variable: "XDG_DATA_HOME",
    what: "the instance's database",
    xdg_tail: "iaam/iaam.db",
    home_tail: ".local/share/iaam/iaam.db",
    allowed: "a path to the database file",
};

const BROKER_KEY_PLACE: PlaceSpec = PlaceSpec {
    variable: "IAAM_BROKER_KEY_FILE",
    xdg_variable: "XDG_CONFIG_HOME",
    what: "the broker key file",
    xdg_tail: "iaam/broker-key",
    home_tail: ".config/iaam/broker-key",
    allowed: "a path to the key file",
};

/// Resolves the places of the instance from one lookup. This is the one
/// resolver: every command's places come from here, and no command
/// resolves a path from the environment on its own.
///
/// The database place must resolve. The key place resolves when it can;
/// when it cannot, the reason rides in `Places::broker_key` for the
/// commands that need the key — and for `iaam status`, which prints it —
/// while the commands that never touch the key go on without a key place.
fn resolve_places<F>(get: &F) -> Result<Places, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    Ok(Places {
        database: resolve_place(&DATABASE_PLACE, get)?,
        broker_key: resolve_place(&BROKER_KEY_PLACE, get),
    })
}

/// Resolves one place of the instance: the override variable wins, else the
/// XDG directory when it is set and absolute, else the home directory. The
/// XDG spec counts a relative value as unset, and the same rule applies to
/// `HOME` — a place hanging off a relative path would move with the working
/// directory. An override that is set but empty is invalid input and an
/// error, never a silent default. A path is bytes: a value the operating
/// system allows is a path here even when it is not UTF-8.
fn resolve_place<F>(spec: &PlaceSpec, get: &F) -> Result<Place, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(value) = get(spec.variable) {
        if value.is_empty() {
            return Err(ConfigError::Invalid {
                name: spec.variable,
                value: value.to_string_lossy().into_owned(),
                allowed: spec.allowed,
            });
        }
        return Ok(Place {
            path: PathBuf::from(value),
            source: PlaceSource::Variable,
        });
    }
    let default = |base: OsString, tail: &'static str| Place {
        path: PathBuf::from(base).join(tail),
        source: PlaceSource::Default,
    };
    if let Some(xdg) = absolute(get(spec.xdg_variable)) {
        return Ok(default(xdg, spec.xdg_tail));
    }
    if let Some(home) = absolute(get("HOME")) {
        return Ok(default(home, spec.home_tail));
    }
    Err(ConfigError::NoPlace {
        what: spec.what,
        variable: spec.variable,
        xdg_variable: spec.xdg_variable,
    })
}

/// A lookup result that is present and usable as a place's base: non-empty
/// and absolute. Absolute is decided on the bytes, so a home directory
/// whose name is not UTF-8 still supplies the default place.
fn absolute(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty() && Path::new(value).is_absolute())
}

/// A setting that is text and not a path, as text: when the variable holds
/// bytes that are not UTF-8, that is invalid input and refused naming the
/// variable — never a silent default.
fn text_setting(
    name: &'static str,
    value: Option<OsString>,
    allowed: &'static str,
) -> Result<Option<String>, ConfigError> {
    value
        .map(|value| {
            value.into_string().map_err(|bytes| ConfigError::Invalid {
                name,
                value: bytes.to_string_lossy().into_owned(),
                allowed,
            })
        })
        .transpose()
}

impl Places {
    pub(crate) fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        resolve_places(&get)
    }

    /// The key place, for the commands that need the key: `generate` writes
    /// it, the access commands and `serve` read it. A command that does not
    /// need the key never calls this, so a key place that does not exist
    /// cannot stand between the owner and `iaam claim`.
    pub(crate) fn key_place(&self) -> Result<&Place, ConfigError> {
        self.broker_key.as_ref().map_err(|error| error.clone())
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The database file of the instance, and who chose its place.
    pub database: Place,
    /// File containing the encryption key for broker access, and who chose
    /// its place.
    ///
    /// The key has a default place on purpose, and the place is deliberately
    /// **apart from the data**: `$XDG_CONFIG_HOME/iaam/broker-key`, or under
    /// the home directory. Copying or backing up the data directory then
    /// cannot carry the key with it, and `iaam status` shows where the key
    /// actually is. The default does not publish the key — the file is
    /// created mode `0600` inside a directory mode `0700` — and an explicit
    /// `IAAM_BROKER_KEY_FILE` still wins where a deployment names its own
    /// place (systemd credentials, a container mount).
    pub broker_key: Place,
    /// A directory of source profiles the operator supplies himself.
    ///
    /// Optional, and with no default on its own ground: a profile decides
    /// how every future row of one institution's format is read, so one
    /// picked up from a known place would be one nobody chose. Absent, the
    /// instance reads documents with the profiles this build ships and no
    /// others — which is a complete catalogue, not a degraded one.
    ///
    /// The directory is read once, at start-up, and only `.json` files in it
    /// are considered. A file that is not a valid profile, and a local
    /// profile whose id collides with a bundled one, are **refused and
    /// published as refused** rather than skipped: a profile that is merely
    /// absent looks exactly like one that was never written.
    pub source_profiles: Option<PathBuf>,
    pub listen: SocketAddr,
    pub rate_limit: u32,
    pub rate_window: Duration,
}

impl Config {
    /// Read configuration from the environment through `from_lookup`, the
    /// seam every caller here uses. The full configuration belongs to the
    /// commands whose work may need the key — `serve` and the broker-access
    /// commands — so the key place must resolve here; the commands that
    /// need the places alone read [`Places::from_lookup`], where it need
    /// not. What no default ever does is create anything: only `iaam claim`
    /// creates the database, and only `iaam broker key generate` creates
    /// the key.
    pub(crate) fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let Places {
            database,
            broker_key,
        } = resolve_places(&get)?;
        let listen = text_setting(
            "IAAM_LISTEN",
            get("IAAM_LISTEN"),
            "socket address such as 127.0.0.1:8080",
        )?
        .unwrap_or_else(|| "127.0.0.1:8080".into());
        let listen = listen.parse().map_err(|_| ConfigError::Invalid {
            name: "IAAM_LISTEN",
            value: listen,
            allowed: "socket address such as 127.0.0.1:8080",
        })?;
        let rate_limit = parse_u32("IAAM_RATE_LIMIT", 120, &get)?;
        let rate_window =
            Duration::from_secs(u64::from(parse_u32("IAAM_RATE_WINDOW_SECONDS", 60, &get)?));

        Ok(Self {
            database,
            broker_key: broker_key?,
            source_profiles: get("IAAM_SOURCE_PROFILES").map(PathBuf::from),
            listen,
            rate_limit,
            rate_window,
        })
    }
}

fn parse_u32<F>(name: &'static str, default: u32, get: &F) -> Result<u32, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    match text_setting(name, get(name), "integer from 0 to 4294967295")? {
        None => Ok(default),
        Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
            name,
            value,
            allowed: "integer from 0 to 4294967295",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, PlaceSource, Places};
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn values<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    /// The lookup as the operating system supplies it: values are bytes,
    /// and a path variable may hold any of them.
    fn raw_values<'a>(
        values: &'a [(&'a str, &'a OsString)],
    ) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).clone())
        }
    }

    #[test]
    fn database_variable_overrides_the_default_place() {
        let config = Config::from_lookup(values(&[
            ("IAAM_DATABASE", "/var/lib/iaam/iaam.db"),
            ("HOME", "/home/dev"),
        ]))
        .unwrap();

        assert_eq!(
            config.database.path,
            std::path::Path::new("/var/lib/iaam/iaam.db")
        );
        assert_eq!(config.database.source, PlaceSource::Variable);
    }

    #[test]
    fn database_default_place_follows_absolute_xdg_data_home() {
        let config = Config::from_lookup(values(&[
            ("XDG_DATA_HOME", "/xdata"),
            ("HOME", "/home/dev"),
        ]))
        .unwrap();

        assert_eq!(
            config.database.path,
            std::path::Path::new("/xdata/iaam/iaam.db")
        );
        assert_eq!(config.database.source, PlaceSource::Default);
    }

    #[test]
    fn database_default_place_follows_home_without_xdg() {
        let config = Config::from_lookup(values(&[("HOME", "/home/dev")])).unwrap();

        assert_eq!(
            config.database.path,
            std::path::Path::new("/home/dev/.local/share/iaam/iaam.db")
        );
        assert_eq!(config.database.source, PlaceSource::Default);
    }

    #[test]
    fn relative_xdg_values_are_ignored_as_the_xdg_spec_says() {
        let config = Config::from_lookup(values(&[
            ("XDG_DATA_HOME", "relative/data"),
            ("XDG_CONFIG_HOME", "relative/config"),
            ("HOME", "/home/dev"),
        ]))
        .unwrap();

        assert_eq!(
            config.database.path,
            std::path::Path::new("/home/dev/.local/share/iaam/iaam.db")
        );
        assert_eq!(
            config.broker_key.path,
            std::path::Path::new("/home/dev/.config/iaam/broker-key")
        );
    }

    #[test]
    fn broker_key_variable_overrides_the_default_place() {
        let config = Config::from_lookup(values(&[
            ("HOME", "/home/dev"),
            ("IAAM_BROKER_KEY_FILE", "/etc/iaam/broker-key"),
        ]))
        .unwrap();

        assert_eq!(
            config.broker_key.path,
            std::path::Path::new("/etc/iaam/broker-key")
        );
        assert_eq!(config.broker_key.source, PlaceSource::Variable);
    }

    #[test]
    fn broker_key_default_place_follows_absolute_xdg_config_home() {
        let config = Config::from_lookup(values(&[
            ("XDG_CONFIG_HOME", "/xconfig"),
            ("HOME", "/home/dev"),
        ]))
        .unwrap();

        assert_eq!(
            config.broker_key.path,
            std::path::Path::new("/xconfig/iaam/broker-key")
        );
        assert_eq!(config.broker_key.source, PlaceSource::Default);
    }

    #[test]
    fn no_home_and_no_override_names_the_variables_that_would_supply_the_place() {
        let error = Config::from_lookup(values(&[])).unwrap_err();

        assert!(matches!(error, ConfigError::NoPlace { .. }));
        let text = error.to_string();
        assert!(text.contains("IAAM_DATABASE"), "{text}");
        assert!(text.contains("XDG_DATA_HOME"), "{text}");
        assert!(text.contains("HOME"), "{text}");
        assert!(!text.contains("NoPlace {"));
    }

    #[test]
    fn no_home_names_the_key_variables_when_the_key_has_no_override() {
        // The database resolves; the key place comes back with the reason,
        // which names every variable that would supply it.
        let places =
            Places::from_lookup(values(&[("IAAM_DATABASE", "/var/lib/iaam/iaam.db")])).unwrap();
        let error = places.broker_key.unwrap_err();

        assert!(matches!(error, ConfigError::NoPlace { .. }));
        let text = error.to_string();
        assert!(text.contains("IAAM_BROKER_KEY_FILE"), "{text}");
        assert!(text.contains("XDG_CONFIG_HOME"), "{text}");
        assert!(text.contains("HOME"), "{text}");
    }

    #[test]
    fn an_explicit_database_does_not_need_a_key_place() {
        // The documented container invocation: an explicit database and no
        // home directory. The places resolve; the key place is the only one
        // that may come back with a reason instead of a path.
        let places = Places::from_lookup(values(&[("IAAM_DATABASE", "/var/lib/iaam/iaam.db")]));
        let places = places.unwrap();
        assert_eq!(
            places.database.path,
            std::path::Path::new("/var/lib/iaam/iaam.db")
        );
        assert_eq!(places.database.source, PlaceSource::Variable);
    }

    #[test]
    fn empty_override_variable_is_an_error_not_a_default() {
        let error = Config::from_lookup(values(&[("IAAM_DATABASE", ""), ("HOME", "/home/dev")]))
            .unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Invalid {
                name: "IAAM_DATABASE",
                ..
            }
        ));
        let text = error.to_string();
        assert!(text.contains("IAAM_DATABASE"), "{text}");
        assert!(text.contains("invalid"), "{text}");
    }

    #[test]
    fn places_resolve_without_the_serve_settings() {
        // `iaam status` and the key command answer for the places alone: a
        // broken serve setting must not stand between the owner and the
        // answer to "where is my instance".
        let places = Places::from_lookup(values(&[
            ("HOME", "/home/dev"),
            ("IAAM_LISTEN", "not an address"),
            ("IAAM_RATE_LIMIT", "zero"),
        ]))
        .unwrap();

        assert_eq!(
            places.database.path,
            std::path::Path::new("/home/dev/.local/share/iaam/iaam.db")
        );
        assert_eq!(
            places.broker_key.unwrap().path,
            std::path::Path::new("/home/dev/.config/iaam/broker-key")
        );
    }

    #[test]
    fn invalid_listen_value_names_allowed_form() {
        let error = Config::from_lookup(values(&[
            ("IAAM_DATABASE", "db.sqlite"),
            ("IAAM_LISTEN", "not an address"),
            ("HOME", "/home/dev"),
        ]))
        .unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Invalid {
                name: "IAAM_LISTEN",
                ..
            }
        ));
        let text = error.to_string();
        assert!(text.contains("IAAM_LISTEN"));
        assert!(text.contains("allowed values"));
        assert!(text.contains("socket address"));
        assert!(!text.contains("Invalid {"));
    }

    #[test]
    fn a_non_utf8_database_variable_is_the_database_not_a_default_place() {
        // `/` followed by one byte that is not UTF-8: a path the operating
        // system allows and `std::env::var` cannot carry.
        let raw = OsString::from_vec(vec![0x2f, 0xff]);
        let places = Places::from_lookup(raw_values(&[
            ("IAAM_DATABASE", &raw),
            ("HOME", &OsString::from("/home/dev")),
        ]))
        .unwrap();

        assert_eq!(places.database.path, std::path::PathBuf::from(raw));
        assert_eq!(places.database.source, PlaceSource::Variable);
    }

    #[test]
    fn a_non_utf8_home_still_supplies_the_default_place() {
        let home = OsString::from_vec(vec![0x2f, 0xff]);
        let places = Places::from_lookup(raw_values(&[("HOME", &home)])).unwrap();

        assert_eq!(
            places.database.path,
            std::path::Path::new(&home).join(".local/share/iaam/iaam.db")
        );
        assert_eq!(places.database.source, PlaceSource::Default);
    }

    #[test]
    fn a_non_utf8_serve_setting_is_refused_not_a_default() {
        let raw = OsString::from_vec(vec![0xff]);
        let error = Config::from_lookup(raw_values(&[
            ("IAAM_LISTEN", &raw),
            ("HOME", &OsString::from("/home/dev")),
        ]))
        .unwrap_err();

        assert!(
            matches!(
                error,
                ConfigError::Invalid {
                    name: "IAAM_LISTEN",
                    ..
                }
            ),
            "{error}"
        );
    }
}
