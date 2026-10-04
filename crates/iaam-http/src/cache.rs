//! The response cache: an answer the gateway already holds is not asked
//! for again.
//!
//! **The rule — what is cached.** A request is cached only when the request
//! itself says it reads, and never otherwise:
//!
//! - an HTTP `GET` is a read;
//! - a request the source marked safe to repeat ([`HttpRequest::idempotent`])
//!   is a read: T-Invest reads are POSTs to `…Service/Get…`, and CBR's SOAP
//!   queries are POSTs for the same reason;
//! - a credential exchange is never a read, whatever its method marks.
//!   Finam's `POST /v1/sessions` and `/v1/sessions/details` carry the
//!   secret or the session token in the body, and any request whose body
//!   names a credential — `secret`, `token` or `password` anywhere in its
//!   text — is treated the same way. The rule is deliberately broad: a body
//!   refused the cache is sent again, which is the ordinary path; a body
//!   wrongly cached would put a credential beside the database.
//!
//! Only an answer with a 2xx status is stored, and the stored answer —
//! status, body and the headers [`HttpResponse`] carries — stands in for
//! the send for [`CACHE_TTL`], one hour.
//!
//! **The moment a served answer dates.** The entry keeps the storage stamp
//! the hour of validity is measured from and, when the stored answer
//! already carried its own observation moment (the stamp the real client
//! gave it at status-line time), that moment too. A served answer is
//! dated by the moment it was observed, not by the moment the store wrote
//! it: a response whose headers arrived just before midnight and whose
//! body finished just after is observed on one day when served, whatever
//! day the store was written on. The moment line is optional — an entry
//! written before it existed still parses, and the served answer is then
//! dated by its storage stamp as it always was.
//!
//! **The key.** The SHA-256 of the destination, the method, the wire URL
//! (path and full query), the request body and a SHA-256 fingerprint of
//! the caller-bound stable access identity where one is set (Finam binds
//! its long-lived broker secret to every authorized read), otherwise of
//! the presented credential. One access's answer never serves another, and
//! no file of the cache ever holds a credential: the fingerprint is a
//! digest, and the key is a digest of everything, the fingerprint included.
//!
//! **The place and the bounds.** The cache is the directory
//! `<database>.cache` beside the instance's database — the same
//! derivation the egress directory uses — created 0700 with 0600 entry
//! files, so it is private to the owner and it survives a restart. Every
//! access removes the entries older than the hour, and the store holds at
//! most [`MAX_ENTRIES`] entries: when it is full, the oldest answers go
//! first, so the cache never grows without bound. A body larger than
//! [`MAX_STORED_BODY`] is not stored at all. Clearing the cache is
//! deleting the directory; the next answer starts it again.

use core::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::destination::Destination;
use crate::egress::{EgressDirectoryError, database_directory_for};
use crate::gateway::{Clock, redacted_bytes, without_secret};
use crate::request::{HttpMethod, HttpRequest, Secret};
use crate::response::HttpResponse;

/// How long a stored answer stands in for a send: the owner's one hour.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// [`CACHE_TTL`] in the nanoseconds the stamps are counted in.
const CACHE_TTL_NANOS: u64 = CACHE_TTL.as_nanos() as u64;

/// The most entries the cache keeps. A store that would exceed it removes
/// the oldest answers first, so an hour of distinct reads still bounds the
/// store whatever the access pattern was.
const MAX_ENTRIES: usize = 1024;

/// The largest body stored. A bigger answer is sent again rather than kept:
/// the cache serves ordinary reads, not an archive of bulk pages.
const MAX_STORED_BODY: usize = 8 * 1024 * 1024;

/// The first line of every entry file; a file that does not start with it
/// is not an answer and is removed.
const ENTRY_VERSION: &str = "iaam-response-cache-v1";

/// The response cache could not be placed or created beside the database.
#[derive(Debug, Error)]
pub(crate) enum CacheError {
    #[error(
        "the response cache cannot live beside the database {database}: {reason}",
        database = .database.display()
    )]
    Place { database: PathBuf, reason: String },
    #[error(
        "the response cache directory {path} could not be created: {source}; the instance needs leave to keep its cache beside the database",
        path = .path.display()
    )]
    Directory {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Where one instance's response cache lives: the directory named after its
/// database (`iaam.sqlite` → `iaam.sqlite.cache`), the same derivation the
/// egress directory uses, so every alias of the database is one cache.
///
/// # Errors
/// `CacheError::Place` when the database does not resolve to an existing
/// file, has no file name, or has more than one name.
pub(crate) fn cache_directory_for(database: &Path) -> Result<PathBuf, CacheError> {
    let place = database_directory_for(database, ".cache").map_err(|error| {
        let reason = match &error {
            EgressDirectoryError::DatabaseUnresolved { source, .. } => format!(
                "the database does not exist, so the cache cannot be placed beside it ({source}); \
                 the gateway is built after the store exists, so check which database this process was given"
            ),
            EgressDirectoryError::NoFileName { .. } => {
                "the database path has no file name, so the cache directory cannot be named after it"
                    .to_owned()
            }
            EgressDirectoryError::HardLinked { links, .. } => format!(
                "the database has {links} names (hard links); each name would keep its own cache, \
                 so the cache is refused: keep one name and remove the others (`find -samefile` lists them)"
            ),
        };
        CacheError::Place {
            database: database.to_owned(),
            reason,
        }
    })?;
    Ok(place)
}

/// The key a stored answer sits under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheKey([u8; 32]);

impl CacheKey {
    /// The key of `request`, or `None` when the request is not cached: it
    /// is not a read, or it is a credential exchange.
    #[must_use]
    pub(crate) fn of(request: &HttpRequest) -> Option<Self> {
        if !is_read(request) || is_credential_exchange(request) {
            return None;
        }
        Some(Self(key_digest(request)))
    }

    /// The entry file name: the key in hexadecimal. The name carries
    /// nothing else, and a digest carries no credential.
    fn hex(&self) -> String {
        self.0
            .iter()
            .fold(String::with_capacity(64), |mut text, byte| {
                let _ = write!(text, "{byte:02x}");
                text
            })
    }
}

/// Whether the request only reads, by what it itself declares: an HTTP
/// `GET`, or a request the source marked safe to repeat — T-Invest reads
/// are POSTs to `…Service/Get…`, CBR's SOAP queries POSTs for the same
/// reason.
fn is_read(request: &HttpRequest) -> bool {
    matches!(request.method(), HttpMethod::Get) || request.is_idempotent()
}

/// Whether the request exchanges a credential: Finam's session exchange
/// and its details call by name — the published contract puts the secret
/// and the token in their bodies — and so does any request whose body
/// names a credential anywhere.
fn is_credential_exchange(request: &HttpRequest) -> bool {
    if matches!(request.destination(), Destination::FinamApi)
        && matches!(request.path(), "/v1/sessions" | "/v1/sessions/details")
    {
        return true;
    }
    request
        .body()
        .is_some_and(|body| mentions_credential(body.payload()))
}

/// Whether the body text names a credential, whatever the spelling: JSON
/// keys and XML tags both land in the text, and the comparison ignores
/// case. Deliberately broad — a body refused the cache is merely sent
/// again, while a body wrongly cached would put a credential beside the
/// database.
fn mentions_credential(payload: &str) -> bool {
    const WORDS: [&str; 3] = ["secret", "token", "password"];
    let bytes = payload.as_bytes();
    WORDS.iter().any(|word| {
        let lower = word.as_bytes();
        bytes
            .windows(lower.len())
            .any(|window| window.eq_ignore_ascii_case(lower))
    })
}

fn key_digest(request: &HttpRequest) -> [u8; 32] {
    let mut material = Vec::new();
    field(&mut material, request.destination().base_url().as_bytes());
    field(&mut material, method_name(request.method()).as_bytes());
    field(&mut material, request.url().as_bytes());
    match request.body() {
        Some(body) => {
            field(&mut material, body.content_type().as_bytes());
            field(&mut material, body.payload().as_bytes());
        }
        None => field(&mut material, b"-"),
    }
    // The identity the key names: the caller-bound stable access identity
    // where the caller bound one (Finam binds its long-lived broker secret
    // to every authorized read), else the presented credential — so a
    // renewed session token does not re-send a read the same access
    // already has cached. The identity or credential itself never enters
    // the key: its SHA-256 fingerprint does, so one access's answer never
    // serves another and no file of the cache ever holds a credential.
    // The presented credential is materialized once, so its borrow lives
    // through the match below.
    let credential = request.authorization();
    match request.cache_identity().or(credential.as_ref()) {
        Some(identity) => {
            let fingerprint = Sha256::digest(identity.expose().as_bytes());
            field(&mut material, &fingerprint);
        }
        None => field(&mut material, b"-"),
    }
    let digest = Sha256::digest(&material);
    let mut key = [0_u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// Appends one length-prefixed field, so no concatenation can blur where
/// one field ends and the next begins.
fn field(material: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).expect("a request field fits in 8 bytes");
    material.extend_from_slice(&length.to_be_bytes());
    material.extend_from_slice(bytes);
}

const fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
    }
}

/// The instance's response cache: one directory beside its database, shared
/// by every process the database belongs to, held across restarts.
#[derive(Clone)]
pub(crate) struct ResponseCache {
    directory: PathBuf,
    clock: Arc<dyn Clock>,
}

impl core::fmt::Debug for ResponseCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ResponseCache")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl ResponseCache {
    /// Opens the cache at `place`, creating the directory 0700 when it does
    /// not exist, and removes the entries an earlier process left behind
    /// that have since outlived the hour.
    ///
    /// # Errors
    /// `CacheError::Directory` when the place cannot be created or is not a
    /// directory.
    pub(crate) fn open(place: &Path, clock: Arc<dyn Clock>) -> Result<Self, CacheError> {
        if let Err(source) = std::fs::DirBuilder::new().mode(0o700).create(place) {
            if source.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(CacheError::Directory {
                    path: place.to_owned(),
                    source,
                });
            }
        }
        let metadata = std::fs::metadata(place).map_err(|source| CacheError::Directory {
            path: place.to_owned(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(CacheError::Directory {
                path: place.to_owned(),
                source: std::io::Error::other("the cache place must be a directory"),
            });
        }
        let cache = Self {
            directory: place.to_owned(),
            clock,
        };
        cache.sweep();
        Ok(cache)
    }

    /// The stored answer under `key`, or `None` when there is none: never
    /// stored, already outlived the hour, or unreadable. An entry that has
    /// outlived the hour, and a file that is not a valid entry, are removed
    /// here, so the store keeps only answers that still stand in.
    pub(crate) fn lookup(&self, key: &CacheKey) -> Option<HttpResponse> {
        self.sweep();
        let path = self.directory.join(key.hex());
        let bytes = std::fs::read(&path).ok()?;
        match Entry::parse(&bytes) {
            Ok(entry) => {
                if self.expired(entry.stored) {
                    let _ = std::fs::remove_file(&path);
                    None
                } else {
                    let mut response = entry.response;
                    // The moment the answer was observed: the answer's own
                    // stamp when the entry carries one, else the moment the
                    // entry was stored — so a caller dates a served answer
                    // by its real observation time, not by the serve, and a
                    // response cached across midnight keeps the day it was
                    // actually observed.
                    response.observed_at = Some(match entry.observed_at {
                        Some(at) => at,
                        None => SystemTime::UNIX_EPOCH + Duration::from_nanos(entry.stored),
                    });
                    Some(response)
                }
            }
            Err(()) => {
                // The file is not an answer anybody stored whole: it is
                // removed so the store keeps nothing unreadable.
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Stores the answer under `key`, best effort: a store that cannot be
    /// made leaves the cache as it was, and the next call for the same read
    /// is sent again.
    ///
    /// `bearer` and `identity` are the presented credential and the cache
    /// identity the read was keyed on: before anything is persisted, both
    /// are cut out of the stored headers and body, the same redaction the
    /// log gives a value a destination echoed the token into, so a cache
    /// file never holds a credential.
    pub(crate) fn store(
        &self,
        key: &CacheKey,
        response: &HttpResponse,
        bearer: Option<&Secret>,
        identity: Option<&Secret>,
    ) {
        if response.body.len() > MAX_STORED_BODY {
            return;
        }
        self.sweep();
        self.evict_over_the_cap();
        let name = key.hex();
        let temporary = self.directory.join(format!("{name}.tmp"));
        let entry = Entry::of(response, self.stored_now(), bearer, identity);
        let stored = (|| -> std::io::Result<()> {
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A store that died before its rename left the
                    // half-made file; it is nobody's answer, so it is
                    // replaced.
                    std::fs::remove_file(&temporary)?;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&temporary)?
                }
                Err(source) => return Err(source),
            };
            file.write_all(&entry)?;
            file.flush()?;
            std::fs::rename(&temporary, self.directory.join(&name))
        })();
        if let Err(source) = stored {
            let _ = std::fs::remove_file(&temporary);
            tracing::warn!(
                path = %self.directory.join(&name).display(),
                reason = %source,
                "a cached answer could not be stored"
            );
        }
    }

    /// Removes every entry older than the hour. Runs on every access, so
    /// the store never keeps what it would not serve.
    fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
                continue;
            };
            if !is_entry_name(name) {
                // Half-made stores (`…​.tmp`) and anything foreign are left
                // alone; only answers are swept.
                continue;
            }
            let expired = stamp_of(&path).is_none_or(|stored| self.expired(stored));
            if expired {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Removes the oldest answers while the store holds more than
    /// [`MAX_ENTRIES`] entries, so even a burst of distinct reads inside
    /// the hour leaves the store bounded.
    fn evict_over_the_cap(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return;
        };
        let mut answers: Vec<(u64, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.file_name().to_str().is_some_and(is_entry_name))
            .filter_map(|entry| Some((stamp_of(&entry.path())?, entry.path())))
            .collect();
        if answers.len() < MAX_ENTRIES {
            return;
        }
        answers.sort_by_key(|(stored, _)| *stored);
        let excess = answers.len() + 1 - MAX_ENTRIES;
        for (_, path) in answers.iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn expired(&self, stored: u64) -> bool {
        let now = unix_nanos(self.clock.now_unix());
        stored.saturating_add(CACHE_TTL_NANOS) <= now
    }

    fn stored_now(&self) -> u64 {
        unix_nanos(self.clock.now_unix())
    }
}

fn unix_nanos(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

fn is_entry_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The `stored` stamp from a file's first lines, or `None` when the file
/// does not start like an entry at all.
fn stamp_of(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut prefix = [0_u8; 128];
    let read = file.read(&mut prefix).ok()?;
    let mut lines = prefix[..read].split(|byte| *byte == b'\n');
    let version = lines.next()?;
    if version != ENTRY_VERSION.as_bytes() {
        return None;
    }
    let stored = core::str::from_utf8(lines.next()?).ok()?;
    stored.strip_prefix("stored ")?.parse().ok()
}

/// One stored answer: the header lines, then the body bytes as they were
/// received. `stored` is the stamp the hour of validity is measured from;
/// `observed_at` is the answer's own observation moment, when the stored
/// answer carried one.
struct Entry {
    stored: u64,
    observed_at: Option<SystemTime>,
    response: HttpResponse,
}

impl Entry {
    fn of(
        response: &HttpResponse,
        stored: u64,
        bearer: Option<&Secret>,
        identity: Option<&Secret>,
    ) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(192 + response.body.len());
        line(&mut bytes, ENTRY_VERSION);
        line(&mut bytes, &format!("stored {stored}"));
        // The answer's own observation moment, kept beside the storage
        // stamp when the answer carried one: a served answer dates the day
        // it was observed, not the day the store wrote it.
        if let Some(at) = response.observed_at {
            line(&mut bytes, &format!("observed-at {}", unix_nanos(at)));
        }
        line(&mut bytes, &format!("status {}", response.status));
        line(
            &mut bytes,
            &match response.retry_after {
                Some(delay) => format!("retry-after-ms {}", delay.as_millis()),
                None => "retry-after-ms -".to_owned(),
            },
        );
        line(
            &mut bytes,
            &named(
                "location",
                without_the_credentials(response.location.as_deref(), bearer, identity).as_deref(),
            ),
        );
        line(
            &mut bytes,
            &named(
                "content-type",
                without_the_credentials(response.content_type.as_deref(), bearer, identity)
                    .as_deref(),
            ),
        );
        line(
            &mut bytes,
            &named(
                "request-id",
                without_the_credentials(response.request_id.as_deref(), bearer, identity)
                    .as_deref(),
            ),
        );
        // The body too: a destination that echoes the presented token into
        // its answer must not hand it to the next reader of the cache file.
        bytes.extend_from_slice(&redacted_bytes(
            &redacted_bytes(&response.body, bearer),
            identity,
        ));
        bytes
    }

    /// The strict inverse of [`Self::of`]: the version line, the storage
    /// stamp, an optional `observed-at` line when the stored answer
    /// carried its own observation moment, then the five answer header
    /// lines, then the body. Anything else — another version, a short or
    /// repeated line, a carriage return in a header — is not an answer.
    /// An entry written before the `observed-at` line existed (an earlier
    /// build's entry) has none and still parses: the storage stamp is then
    /// followed straight by the status line, and `lookup` dates the served
    /// answer by the storage stamp as it always did.
    fn parse(bytes: &[u8]) -> Result<Self, ()> {
        let mut rest = bytes;
        if line_of(&mut rest)? != ENTRY_VERSION.as_bytes() {
            return Err(());
        }
        let stored = header_field(line_of(&mut rest)?, "stored")?
            .parse()
            .map_err(|_| ())?;
        // The answer's own observation moment, read only when the next
        // line is its `observed-at`; an entry without the line proceeds
        // straight to the status line.
        let mut observed_at = None;
        if let Some(line) = peek_line(rest)
            && line.starts_with(b"observed-at ")
        {
            let nanos = header_field(line_of(&mut rest)?, "observed-at")?
                .parse()
                .map_err(|_| ())?;
            observed_at = Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos));
        }
        let status = header_field(line_of(&mut rest)?, "status")?
            .parse()
            .map_err(|_| ())?;
        let retry_after = match header_field(line_of(&mut rest)?, "retry-after-ms")? {
            "-" => None,
            millis => Some(Duration::from_millis(millis.parse().map_err(|_| ())?)),
        };
        let location = value(line_of(&mut rest)?, "location")?;
        let content_type = value(line_of(&mut rest)?, "content-type")?;
        let request_id = value(line_of(&mut rest)?, "request-id")?;
        let response = HttpResponse {
            status,
            body: rest.to_vec(),
            retry_after,
            location,
            content_type,
            request_id,
            // `lookup` dates the served answer: from the answer's own
            // moment when the entry carries one, else from the storage
            // stamp.
            observed_at: None,
        };
        Ok(Self {
            stored,
            observed_at,
            response,
        })
    }
}

/// The next line and the bytes after it, or `Err` when there is none or
/// the line holds a carriage return.
fn line_of<'a>(rest: &mut &'a [u8]) -> Result<&'a [u8], ()> {
    let newline = rest.iter().position(|byte| *byte == b'\n').ok_or(())?;
    let (line, remainder) = rest.split_at(newline);
    if line.contains(&b'\r') {
        return Err(());
    }
    *rest = &remainder[1..];
    Ok(line)
}

/// The next line, not consumed: what the parser reads to see whether the
/// line after the storage stamp is the answer's own `observed-at` or the
/// status line of an entry written before that line existed.
fn peek_line(rest: &[u8]) -> Option<&[u8]> {
    if rest.is_empty() {
        return None;
    }
    let line = match rest.iter().position(|byte| *byte == b'\n') {
        Some(newline) => &rest[..newline],
        None => rest,
    };
    (!line.contains(&b'\r')).then_some(line)
}

/// `value` with every occurrence of the presented credential and of the
/// cache identity cut out — the same redaction the log applies to a value a
/// destination echoed the token into — or `None` when the header is absent.
/// A header with no occurrence is kept as it was.
fn without_the_credentials(
    value: Option<&str>,
    bearer: Option<&Secret>,
    identity: Option<&Secret>,
) -> Option<String> {
    without_secret(without_secret(value, bearer).as_deref(), identity)
}

fn line(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(b'\n');
}

/// `name value`, or `name -` when the header is absent.
fn named(name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!("{name} {value}"),
        None => format!("{name} -"),
    }
}

fn value(line: &[u8], name: &str) -> Result<Option<String>, ()> {
    let rest = header_field(line, name)?;
    if rest == "-" {
        return Ok(None);
    }
    Ok(Some(rest.to_owned()))
}

/// The value after `name ` in a header line, or `Err` when the line is
/// not the named one or is not UTF-8.
fn header_field<'a>(line: &'a [u8], name: &str) -> Result<&'a str, ()> {
    let prefix = format!("{name} ");
    let rest = line.strip_prefix(prefix.as_bytes()).ok_or(())?;
    core::str::from_utf8(rest).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::destination::Destination;
    use crate::gateway::BootTime;

    /// A clock the test moves by hand in wall time, which is what the
    /// entries are stamped with.
    struct Shifted {
        offset: Mutex<Duration>,
    }

    impl Shifted {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                offset: Mutex::new(Duration::ZERO),
            })
        }

        fn advance(&self, by: Duration) {
            *self.offset.lock().expect("offset") += by;
        }
    }

    impl Clock for Shifted {
        fn now(&self) -> Instant {
            Instant::now()
        }

        fn now_boot(&self) -> Result<BootTime, String> {
            Ok(BootTime::new("cache-test-boot", Duration::ZERO))
        }

        fn now_unix(&self) -> SystemTime {
            SystemTime::now() + *self.offset.lock().expect("offset")
        }
    }

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    /// A cache over an invented instance database in a fresh directory,
    /// with the place returned for inspecting the files.
    fn cache(label: &str, clock: &Arc<Shifted>) -> (ResponseCache, PathBuf) {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "iaam-http-cache-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&parent).expect("test parent created");
        let database = parent.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database written");
        let place = cache_directory_for(&database).expect("cache place derived");
        let cache =
            ResponseCache::open(&place, Arc::clone(clock) as Arc<dyn Clock>).expect("cache opened");
        (cache, place)
    }

    fn answered(status: u16, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_vec(),
            ..Default::default()
        }
    }

    // The request shapes the sources really build.

    fn moex_read() -> HttpRequest {
        HttpRequest::get(
            Destination::MoexIss,
            "/iss/history/engines/stock/markets/bonds/boards/TQCB/securities/SU26238RMFS4.json",
        )
        .with_query("from", "2026-08-01")
    }

    fn tinkoff_read() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            crate::request::RequestBody::Json("{}".to_owned()),
        )
        .with_bearer("test-tinkoff-token")
        .idempotent()
    }

    fn tinkoff_order() -> HttpRequest {
        HttpRequest::post(
            Destination::TinkoffProd,
            "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
            crate::request::RequestBody::Json("{}".to_owned()),
        )
        .with_bearer("test-tinkoff-token")
    }

    fn cbr_soap_read() -> HttpRequest {
        HttpRequest::post(
            Destination::CbrDailyInfo,
            "/DailyInfoWebServ/DailyInfo.asmx",
            crate::request::RequestBody::Xml(
                "<soap:Envelope><KeyRateXML/></soap:Envelope>".to_owned(),
            ),
        )
        .idempotent()
        .with_soap_action("http://web.cbr.ru/KeyRateXML")
    }

    fn finam_read(token: &str) -> HttpRequest {
        HttpRequest::get(Destination::FinamApi, "/v1/accounts/ACC123").with_bare_token(token)
    }

    fn finam_exchange(secret: &str) -> HttpRequest {
        HttpRequest::post(
            Destination::FinamApi,
            "/v1/sessions",
            crate::request::RequestBody::Json(format!(r#"{{"secret":"{secret}"}}"#)),
        )
        .idempotent()
    }

    fn finam_details(token: &str) -> HttpRequest {
        HttpRequest::post(
            Destination::FinamApi,
            "/v1/sessions/details",
            crate::request::RequestBody::Json(format!(r#"{{"token":"{token}"}}"#)),
        )
        .idempotent()
    }

    fn contract_read() -> HttpRequest {
        HttpRequest::get(
            Destination::TinvestContract,
            "/Tinkoff/invest-public-api/master/operations.proto",
        )
    }

    #[test]
    fn reads_are_cached_and_everything_else_is_not() {
        for request in [
            moex_read(),
            cbr_soap_read(),
            contract_read(),
            finam_read("test-token"),
            tinkoff_read(),
        ] {
            assert!(
                CacheKey::of(&request).is_some(),
                "{:?} is a read and is cached",
                request.url()
            );
        }
        for request in [
            tinkoff_order(),
            finam_exchange("test-finam-secret"),
            finam_details("test-session-token"),
        ] {
            let why = if request
                .body()
                .is_some_and(|body| mentions_credential(body.payload()))
            {
                format!("the body carries a credential: {:?}", request.path())
            } else {
                format!("{:?} may act and is not cached", request.path())
            };
            assert!(CacheKey::of(&request).is_none(), "{why}");
        }
    }

    #[test]
    fn a_body_naming_any_credential_is_not_cached_on_any_destination() {
        for body in [
            r#"{"token":"x"}"#,
            r#"{"secret":"x"}"#,
            r#"{"Password":"x"}"#,
            r#"{"note":"the token is here"}"#,
        ] {
            let request = HttpRequest::post(
                Destination::TinkoffSandbox,
                "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
                crate::request::RequestBody::Json(body.to_owned()),
            )
            .with_bearer("test-tinkoff-token")
            .idempotent();
            assert!(
                CacheKey::of(&request).is_none(),
                "a body naming a credential is never cached: {body}"
            );
        }
    }

    #[test]
    fn the_key_separates_destination_url_body_and_credential() {
        let key = |request: &HttpRequest| CacheKey::of(request).expect("a read has a key");
        let moex = moex_read();
        assert_eq!(key(&moex), key(&moex_read()), "the same read, one key");

        assert_ne!(
            key(&moex),
            key(&moex_read().with_query("start", "2")),
            "the full query is in the key"
        );
        assert_ne!(
            key(&moex),
            key(&HttpRequest::get(
                Destination::CbrScripts,
                "/scripts/XML_daily.asp"
            )),
            "the destination is in the key"
        );
        assert_ne!(
            key(&finam_read("test-token-one")),
            key(&finam_read("test-token-two")),
            "the credential fingerprint is in the key"
        );

        let with_body = |body: &str| {
            key(&HttpRequest::post(
                Destination::TinkoffProd,
                "/tinkoff.public.invest.api.contract.v1.UsersService/GetAccounts",
                crate::request::RequestBody::Json(body.to_owned()),
            )
            .with_bearer("test-tinkoff-token")
            .idempotent())
        };
        assert_ne!(
            with_body("{}"),
            with_body(r#"{"status":1}"#),
            "the body is in the key"
        );
    }

    #[test]
    fn the_key_prefers_the_bound_cache_identity_over_the_presented_credential() {
        let key = |request: &HttpRequest| CacheKey::of(request).expect("a read has a key");
        // Two reads with different presented tokens but the same stable
        // access identity share one key: a renewed session token must not
        // re-send a read the same access already has cached within the hour.
        assert_eq!(
            key(&finam_read("session-token-one").with_cache_identity("long-lived-access-secret")),
            key(&finam_read("session-token-two").with_cache_identity("long-lived-access-secret")),
            "the rotating token does not enter the key when the identity is set"
        );
        // A different access identity is a different access.
        assert_ne!(
            key(&finam_read("session-token-one").with_cache_identity("access-one")),
            key(&finam_read("session-token-one").with_cache_identity("access-two")),
            "one access's answer never serves another"
        );
        // Without an identity the key falls back to the presented credential.
        assert_ne!(
            key(&finam_read("test-token-one")),
            key(&finam_read("test-token-two")),
            "the presented credential still keys the cache when no identity is bound"
        );
    }

    #[test]
    fn an_entry_round_trips_the_whole_answer() {
        let time = Shifted::new();
        let (cache, place) = cache("round-trip", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        let response = HttpResponse {
            status: 200,
            body: b"<history>pages</history>".to_vec(),
            retry_after: Some(Duration::from_secs(30)),
            location: Some("https://iss.moex.com/elsewhere".to_owned()),
            content_type: Some("application/json; charset=utf-8".to_owned()),
            request_id: Some("moex-req-7".to_owned()),
            observed_at: None,
        };
        cache.store(&key, &response, None, None);
        // The stored answer also carries the moment it was stored.
        let stamped = stamp_of(&place.join(key.hex())).expect("the entry is stamped");
        let mut stored = response;
        stored.observed_at = Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(stamped));
        assert_eq!(cache.lookup(&key), Some(stored));
    }

    #[test]
    fn a_served_answer_keeps_the_moment_it_was_first_fetched() {
        let time = Shifted::new();
        let (cache, place) = cache("moment", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        cache.store(&key, &answered(200, b"page"), None, None);
        let fetched = stamp_of(&place.join(key.hex())).expect("the entry is stamped");

        // Nearly the hour later the lookup is served.
        time.advance(CACHE_TTL - Duration::from_secs(1));
        let served = cache.lookup(&key).expect("the entry is served");
        assert_eq!(
            served.observed_at,
            Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(fetched)),
            "the served answer keeps the moment it was first fetched, not the moment it was served"
        );
    }

    #[test]
    fn a_served_answer_keeps_its_own_observed_moment() {
        let time = Shifted::new();
        let (cache, place) = cache("own-moment", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        // A live answer's stamp, as the real transport gives it at
        // status-line time.
        let moment = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let response = HttpResponse {
            status: 200,
            body: b"page".to_vec(),
            observed_at: Some(moment),
            ..Default::default()
        };
        cache.store(&key, &response, None, None);

        // The same store serves the next gateway the same read: the
        // served answer keeps the moment it was originally observed,
        // exactly — not the moment the store wrote it.
        let second =
            ResponseCache::open(&place, Arc::clone(&time) as Arc<dyn Clock>).expect("opened");
        time.advance(CACHE_TTL - Duration::from_secs(1));
        let served = second.lookup(&key).expect("the entry is served");
        assert_eq!(
            served.observed_at,
            Some(moment),
            "the served answer keeps its own observed moment, not the storage stamp"
        );
    }

    #[test]
    fn an_entry_written_without_the_observed_at_line_still_parses() {
        // The exact layout an entry written before the observed-at line
        // existed has: the storage stamp followed straight by the status.
        let old = b"iaam-response-cache-v1\nstored 1000\nstatus 200\nretry-after-ms -\nlocation -\ncontent-type -\nrequest-id moex-req-7\npage";
        let parsed =
            Entry::parse(old).expect("an entry written without the observed-at line parses");
        assert_eq!(parsed.stored, 1000);
        assert_eq!(parsed.observed_at, None, "an old entry has no own moment");
        assert_eq!(parsed.response.status, 200);
        assert_eq!(parsed.response.retry_after, None);
        assert_eq!(parsed.response.location, None);
        assert_eq!(parsed.response.request_id.as_deref(), Some("moex-req-7"));
        assert_eq!(parsed.response.body, b"page");
    }

    #[test]
    fn an_entry_is_served_within_the_hour_and_expired_after_it() {
        let time = Shifted::new();
        let (cache, place) = cache("expiry", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        cache.store(&key, &answered(200, b"page"), None, None);

        time.advance(CACHE_TTL - Duration::from_secs(1));
        assert!(
            cache.lookup(&key).is_some(),
            "an entry within the hour is served"
        );

        time.advance(Duration::from_secs(2));
        assert_eq!(
            cache.lookup(&key),
            None,
            "an entry past the hour is not served"
        );
        assert!(
            !place.join(key.hex()).exists(),
            "the expired entry is removed from the store"
        );
    }

    #[test]
    fn a_store_of_the_next_answer_sweeps_the_expired_one() {
        let time = Shifted::new();
        let (cache, place) = cache("sweep", &time);
        let first = CacheKey::of(&moex_read()).expect("a GET is cached");
        cache.store(&first, &answered(200, b"old"), None, None);

        time.advance(CACHE_TTL + Duration::from_secs(1));
        let second = CacheKey::of(&moex_read().with_query("start", "2")).expect("a read");
        cache.store(&second, &answered(200, b"new"), None, None);

        assert!(
            !place.join(first.hex()).exists(),
            "the store of the next answer removed the expired entry"
        );
        assert!(place.join(second.hex()).exists());
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_and_is_removed() {
        let time = Shifted::new();
        let (cache, place) = cache("corrupt", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        std::fs::write(place.join(key.hex()), b"neither header nor body")
            .expect("corrupt entry written");

        assert_eq!(cache.lookup(&key), None, "an unreadable file is no answer");
        assert!(
            !place.join(key.hex()).exists(),
            "the unreadable file is removed"
        );
    }

    #[test]
    fn the_store_never_holds_more_than_the_cap() {
        let time = Shifted::new();
        let (cache, place) = cache("cap", &time);
        let keys: Vec<CacheKey> = (0..=MAX_ENTRIES)
            .map(|page| {
                CacheKey::of(&moex_read().with_query("start", &page.to_string())).expect("a read")
            })
            .collect();
        for (index, key) in keys.iter().enumerate() {
            cache.store(
                key,
                &answered(200, format!("page {index}").as_bytes()),
                None,
                None,
            );
        }

        let entries = std::fs::read_dir(&place)
            .expect("cache read")
            .flatten()
            .filter(|entry| is_entry_name(entry.file_name().to_string_lossy().as_ref()))
            .count();
        assert!(
            entries <= MAX_ENTRIES,
            "the store holds {entries} entries, more than the cap {MAX_ENTRIES}"
        );
        assert_eq!(cache.lookup(&keys[0]), None, "the oldest answer went first");
        assert!(
            cache
                .lookup(keys.last().expect("at least one key"))
                .is_some(),
            "the newest answer is kept"
        );
    }

    #[test]
    fn no_file_of_the_cache_holds_a_credential() {
        let time = Shifted::new();
        let (cache, place) = cache("no-secret", &time);
        let bearer = "test-bearer-token-Q1w2e3r4";
        let secret = "test-finam-secret-Z9y8x7w6";
        let key = CacheKey::of(&finam_read(bearer)).expect("a read");
        cache.store(&key, &answered(200, b"holdings"), None, None);
        // The exchange is never cached; nothing of it may land anywhere.
        assert!(CacheKey::of(&finam_exchange(secret)).is_none());

        let mut files = 0;
        for entry in std::fs::read_dir(&place).expect("cache read").flatten() {
            files += 1;
            let bytes = std::fs::read(entry.path()).expect("file read");
            assert!(
                !contains(&bytes, bearer.as_bytes()),
                "the bearer token leaked into {:?}",
                entry.path()
            );
            assert!(
                !contains(&bytes, secret.as_bytes()),
                "the Finam secret leaked into {:?}",
                entry.path()
            );
        }
        assert!(files > 0, "the cache stored something to inspect");
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn the_cache_is_private_to_its_owner() {
        let time = Shifted::new();
        let (cache, place) = cache("modes", &time);
        let key = CacheKey::of(&moex_read()).expect("a GET is cached");
        cache.store(&key, &answered(200, b"x"), None, None);

        let directory_mode = std::fs::metadata(&place)
            .expect("place read")
            .permissions()
            .mode();
        assert_eq!(directory_mode & 0o777, 0o700, "the directory is private");
        let entry_mode = std::fs::metadata(place.join(key.hex()))
            .expect("entry read")
            .permissions()
            .mode();
        assert_eq!(entry_mode & 0o777, 0o600, "the entry is owner-only");
    }

    #[test]
    fn the_cache_place_is_named_after_the_database() {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "iaam-http-cache-place-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&parent).expect("test parent created");
        let database = parent.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database written");

        let place = cache_directory_for(&database).expect("place derived");
        assert_eq!(place, parent.join("iaam.sqlite.cache"));

        let alias = parent.join(".").join("iaam.sqlite");
        assert_eq!(
            cache_directory_for(&alias).expect("alias place derived"),
            place,
            "every alias of the database is one cache"
        );

        let missing = parent.join("no-database.sqlite");
        let refused = cache_directory_for(&missing).expect_err("missing database refused");
        assert!(refused.to_string().contains("does not exist"), "{refused}");
    }

    #[test]
    fn a_hard_linked_database_is_refused_one_cache() {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "iaam-http-cache-links-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&parent).expect("test parent created");
        let database = parent.join("iaam.sqlite");
        std::fs::write(&database, "").expect("database written");
        std::fs::hard_link(&database, parent.join("iaam-second.sqlite")).expect("hard link made");

        let refused =
            cache_directory_for(&parent.join("iaam-second.sqlite")).expect_err("alias refused");
        assert!(refused.to_string().contains("hard links"), "{refused}");
    }
}
