//! The channel that reads an institution's own export (decision 0019).
//!
//! **The output is an import session, not appended facts**, and that is the
//! whole difference from `POST /v1/documents`. A broker report is a table of
//! trades that need no classification, so the report path records as it reads. A
//! cash statement is rows whose meaning is still open — was this outflow a fee,
//! is this counterparty an account of his elsewhere, did this positive row bring
//! money in or give it back — and both legs of one transfer have to be able to
//! sit in a session before either is recorded. The questions such a document
//! raises are not a cost of this channel; they are what it is for.
//!
//! Three kinds of knowledge meet here and they come from three places, which is
//! the arrangement `docs/import-boundary.md` §3 describes:
//!
//! - **The format** is the profile: which column holds the sum, that the
//!   timestamp is written one way rather than another, which of the source's
//!   words mean which of iaam's. Shipped, reviewed, and data.
//! - **Which account a printed name is** is the owner's directory, read here and
//!   resolved through decision 0004's tiering. A profile names the column and
//!   never a value.
//! - **What the row was** is settled after this function has finished — by the
//!   directory, by one of the owner's classification rules, or by his answer to
//!   a question the session raised. Nothing in this module concludes it, and
//!   nothing in it could: an [`ObservedRow`] has no operation kind.
//!
//! The document is kept before its rows reach the session, as `upload_report`
//! keeps a broker report and for the same reason: a profile corrected next month
//! must have something to read again, and rows converted on a laptop leave the
//! server holding nothing.

use iaam_core::event::provenance::RawHash;
use iaam_core::ids::{AccountId, ImportSessionId, SourceId};
use iaam_ingest::Rejection;
use iaam_ingest::observation::Intake;
use iaam_ingest::profile::{
    Installed, ProfileCatalogue, ReadContext, ReadOutcome, UnresolvedAccountName, engine,
};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{DocumentToKeep, Principal};
use crate::scenarios::import_session::{AccountDirectory, HeldRow, add_rows};

/// Which profile read a document, and which bytes that profile was.
///
/// The digest travels with the id and the version because a version is a name
/// for a content: without it "the rows version 3 read" is not a set, and the
/// facts a buggy profile wrote are not findable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentity {
    pub id: String,
    pub version: u32,
    pub digest: String,
    pub issuer: String,
    pub origin: String,
}

impl ProfileIdentity {
    fn of(installed: &Installed) -> Self {
        Self {
            id: installed.profile.id().to_owned(),
            version: installed.profile.version(),
            digest: installed.profile.digest().to_owned(),
            issuer: installed.profile.issuer().to_owned(),
            origin: installed.origin.code().to_owned(),
        }
    }
}

/// One record of the document, and what became of it.
///
/// The locator is on both arms and it is the record's own position in the file,
/// so an operator can find the line the refusal is about. **One bad row is one
/// bad row**: an unreadable record is refused by name and its neighbours are
/// read (§10.1), which is also why there is no profile key for "the last two
/// lines are totals" — a totals line is read as a row, fails to be one, and
/// says so.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentRow {
    /// The engine read it, and the session holds it.
    Held { locator: u64, held: HeldRow },
    /// The engine could not read it, so there was nothing to hold.
    Unreadable { locator: u64, rejection: Rejection },
}

impl DocumentRow {
    #[must_use]
    pub const fn locator(&self) -> u64 {
        match self {
            Self::Held { locator, .. } | Self::Unreadable { locator, .. } => *locator,
        }
    }
}

/// What one document became.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentImport {
    pub session: ImportSessionId,
    /// The identifier the kept document is on record under.
    pub source: SourceId,
    /// SHA-256 of the bytes, in hexadecimal. Half of every derived row key, so
    /// re-reading the same document under a corrected profile yields the same
    /// keys and appends nothing until the first import is retracted.
    pub document_hash: String,
    pub profile: ProfileIdentity,
    pub rows: Vec<DocumentRow>,
    /// The account names this document printed that the owner's directory
    /// resolved to no single account, once each, in the order the document
    /// first printed them, with the number of its records that printed each.
    ///
    /// **The same refusals, said once per name instead of once per record.**
    /// Every one of these records is refused individually in [`Self::rows`],
    /// which is the contract and stays the contract; this is what makes the
    /// answer readable when one unknown account accounts for two hundred rows.
    /// It states nothing the rows do not: no account is proposed, none is
    /// created, and no name is interpreted.
    ///
    /// Recorded beside the kept document as well, because it is the only place
    /// these names survive — a refused record never reaches the session — and
    /// because recovering them means reading the document again, which the
    /// outstanding-work queue must not do per document on every reading.
    pub unresolved_accounts: Vec<UnresolvedAccountName>,
}

/// The catalogue this instance publishes.
///
/// A property of the deployment and not of the journal, which is why it is read
/// off the services rather than out of the store, and why no route installs a
/// profile: two instances of one image must read one institution's export the
/// same way.
#[must_use]
pub fn catalogue(services: &AppServices) -> &ProfileCatalogue {
    services.profiles.as_ref()
}

/// Read one document into one session.
///
/// `profile` names the profile to read with, where the caller names one; with
/// none, the catalogue is asked which profile recognises the document, and a
/// document two profiles recognise is refused rather than read by whichever
/// came first. A named profile that does not recognise the document is refused
/// too: the alternative is a bank's export read through another bank's columns,
/// which does not half-work — it produces rows.
///
/// `account` is the caller's declaration, needed by a profile whose document is
/// one account's statement and does not print which. Where the caller names
/// none, the session's own declared account stands in, because that is the same
/// statement made once at the session rather than once per document.
///
/// **The scope gate is not here** (`iaam-1tij`). The authority this call
/// demands is `required_scope(OperationKey::ReadImportDocument)`, and the route
/// is gated by asking that function before it enters this module — so what an
/// agent may convey is stated once, where every other call in this API states
/// it, and the queue that offers this call reads the same statement. A
/// `may_submit` test written back in here would be the second statement
/// decision 0021 exists to remove.
pub async fn read_into_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    bytes: &[u8],
    profile: Option<&str>,
    account: Option<AccountId>,
) -> Result<DocumentImport, AppError> {
    let catalogue = catalogue(services);
    let installed = match profile {
        None => catalogue.recognise(bytes).map_err(rejected)?,
        Some(id) => {
            let installed = catalogue.get(id).ok_or_else(|| AppError::Invalid {
                field: "profile".into(),
                expected: "a source profile this instance has installed".into(),
                actual: id.to_owned(),
            })?;
            if !engine::recognises(bytes, &installed.profile) {
                return Err(AppError::Invalid {
                    field: "profile".into(),
                    expected: format!(
                        "a profile that recognises this document. «{id}» reads a document \
                         whose header row carries: {}",
                        installed.profile.recognised_by().join(", ")
                    ),
                    actual: format!("«{id}», which does not recognise the document sent"),
                });
            }
            installed
        }
    };

    // The session is read before the document is kept, and for two reasons.
    // Its declaration stands in for a document that does not print its account,
    // so a caller that declared nothing anywhere is refused rather than left to
    // discover it a row at a time; and a session that is not there is refused
    // before a file is stored under a reading that will not happen.
    let view = services
        .store
        .load_import_session(principal.owner, session)
        .await?
        .ok_or_else(|| AppError::NotFound {
            what: "import session",
            id: session.inner().to_string(),
        })?;
    let declared = account.or(view.account);
    let directory = AccountDirectory::load(services, principal.owner).await?;
    let names = directory.names();
    let reading = engine::read(
        bytes,
        &installed.profile,
        &ReadContext {
            accounts: &names,
            declared,
        },
    )
    .map_err(rejected)?;

    // Kept before the rows reach the session, and under the version that read
    // it: a failed or corrected reading must have something to try again from,
    // and a document stored with no reader named could not say which reading
    // produced the rows beside it.
    let document_hash =
        RawHash::parse(&reading.digest).expect("a SHA-256 digest is 64 hexadecimal characters");
    let source = services
        .store
        .keep_document(DocumentToKeep {
            id: SourceId::new_random(),
            owner: principal.owner,
            broker: installed.profile.issuer().to_owned(),
            format: installed.profile.id().to_owned(),
            parser_version: installed.profile.parser_version(),
            document_hash: document_hash.clone(),
            body: bytes.to_vec(),
        })
        .await?;

    // Every row the engine read, with the profile named on it. This is the one
    // place the reader is stated: the DTO conversion cannot state it, so a
    // caller cannot claim a profile's version for rows it typed by hand, and
    // the rows one profile version wrote stay a set that can be retracted.
    let reader = installed.profile.parser_version();
    let mut fed: Vec<Intake> = Vec::new();
    let mut locators: Vec<u64> = Vec::new();
    let mut unreadable: Vec<DocumentRow> = Vec::new();
    for outcome in &reading.rows {
        match outcome {
            ReadOutcome::Observed { locator, row } => {
                locators.push(*locator);
                fed.push(Intake::Observed {
                    row: row.clone(),
                    reader: Some(reader.clone()),
                });
            }
            ReadOutcome::Rejected { locator, rejection } => {
                unreadable.push(DocumentRow::Unreadable {
                    locator: *locator,
                    rejection: rejection.clone(),
                })
            }
        }
    }
    let held = add_rows(services, principal, session, &fed).await?;

    let mut rows: Vec<DocumentRow> = locators
        .into_iter()
        .zip(held)
        .map(|(locator, held)| DocumentRow::Held { locator, held })
        .collect();
    rows.extend(unreadable);
    // The document's own order, whatever became of each record. A caller
    // comparing the response with the file it sent should not have to sort.
    rows.sort_by_key(DocumentRow::locator);

    // What this reading could not place, kept as an instance fact.
    //
    // Written after the rows have been fed, so that a reading which failed to
    // reach the session leaves no record claiming it happened; and written even
    // when the list is empty, because an empty list is the statement that this
    // reading placed every account the document named — which is what clears
    // what an earlier reading of the same document recorded.
    //
    // Nothing is appended to the journal here, and nothing could be: the
    // records these names came from were refused, so there is no movement to
    // record and no provenance to hang a name on. Decision 0024 says why the
    // fact is the instance's and why the queue reads it here rather than
    // re-reading every kept document.
    services
        .store
        .record_unresolved_accounts(
            principal.owner,
            document_hash,
            session,
            reading.unresolved_accounts.clone(),
        )
        .await?;

    Ok(DocumentImport {
        session,
        source,
        document_hash: reading.digest,
        profile: ProfileIdentity::of(installed),
        rows,
        unresolved_accounts: reading.unresolved_accounts,
    })
}

/// Read a document the instance already kept, under whatever profile now reads
/// it.
///
/// This is the second half of decision 0019 §5's remedy, and without it the
/// first half is a promise nobody can keep: when a profile is corrected,
/// nothing is rewritten, so the way to get the rows right is to retract the
/// import through `POST /v1/corrections/imports` and then **read the same
/// document again** under the new version. The bytes were kept at the first
/// reading precisely so that the owner does not have to still hold the file.
///
/// The row keys come out the same, because they are over the document and the
/// line and nothing else. So a re-read before the retraction appends nothing
/// and is answered `duplicate`, which is the deliberate ordering: putting the
/// profile version into the key would let both imports stand at once and double
/// a month of movements while the owner read a green response.
///
/// A caller who may not submit must not learn from the refusal whether a
/// document with this hash exists (§14), which is why the scope is settled
/// before this function is entered at all: the route asks
/// `required_scope(OperationKey::ReadImportDocument)` before it decides which
/// of the two readings the request names. This function looks the document up
/// as its first act, so a gate placed inside it — here or in
/// [`read_into_session`] — would be the thing that has to be got right twice.
pub async fn reread_into_session(
    services: &AppServices,
    principal: &Principal,
    session: ImportSessionId,
    document_hash: &str,
    profile: Option<&str>,
    account: Option<AccountId>,
) -> Result<DocumentImport, AppError> {
    let Some(requested) = RawHash::parse(document_hash) else {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "SHA-256 of a document this instance kept".into(),
            actual: document_hash.to_owned(),
        });
    };
    let body = services
        .store
        .load_document_body(principal.owner, requested)
        .await?
        .ok_or_else(|| AppError::Invalid {
            field: "document".into(),
            expected: "a document this instance kept, or its bytes in the request body".into(),
            actual: "nothing stored under this hash, or it belongs to another owner".into(),
        })?;
    read_into_session(services, principal, session, &body, profile, account).await
}

/// A refusal about the document, in the shape the transport publishes.
///
/// The engine speaks [`Rejection`] because that is what a row refusal is, and a
/// document refusal has the same three parts for the same reason: the field,
/// what was admissible, and what arrived.
fn rejected(rejection: Rejection) -> AppError {
    AppError::Invalid {
        field: rejection.field,
        expected: rejection.expected,
        actual: rejection.actual,
    }
}
