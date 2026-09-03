//! Uploading and re-parsing reports.
//!
//! The report parser remains in `iaam-ingest`; this scenario only selects
//! it, records the parsed transactions and returns the outcome for each row.

use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_ingest::Verdict;
use iaam_ingest::csv_source::{Directory, ParsedRow};
use iaam_ingest::dedup::IdentityScope;
use iaam_ingest::operation::{NormalizationContext, normalize};
use iaam_ingest::report::finam::FinamParser;
use iaam_ingest::report::tinkoff::TinkoffParser;
use iaam_ingest::report::workbook::Workbook;
use iaam_ingest::report::{Broker, ParsedReport, ReportFormat as ParsedFormat, ReportParser};
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{DocumentToKeep, Principal};
/// A single upload outcome with the row number from the source sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRowVerdict {
    pub row: u64,
    pub verdict: Verdict,
}

/// Document parsing result.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadedDocument {
    pub document_hash: String,
    pub source: SourceId,
    pub broker: Broker,
    pub format: ParsedFormat,
    pub parser_version: ParserVersion,
    pub period: Option<iaam_core::reconciliation::claim::AssertionPeriod>,
    pub rows: Vec<DocumentRowVerdict>,
}

/// Parses and records a report without rejecting the document because of one bad row.
pub async fn upload_report(
    services: &AppServices,
    principal: &Principal,
    bytes: &[u8],
    directory: &Directory,
    account: Option<AccountId>,
) -> Result<UploadedDocument, AppError> {
    require_submit(principal)?;

    let workbook = Workbook::open(bytes).map_err(|error| AppError::Invalid {
        field: "document".into(),
        expected: "supported report workbook".into(),
        actual: error.to_string(),
    })?;
    let parser = detect_parser(&workbook)?;
    let parser_version = parser.version();
    let broker = parser.broker();
    let format = parser.format();
    let report = parser.parse(&workbook, directory);
    if account.is_none() && !report.sections.claims().is_empty() {
        return Err(AppError::Invalid {
            field: "account".into(),
            expected: "account identifier for control sections".into(),
            actual: "not specified".into(),
        });
    }
    let raw_hash = hex_hash(bytes);
    // The document is kept before its rows become facts, and the identifier it
    // is kept under is the one the facts carry. Parsing first and storing
    // afterwards would leave a failed parse with no source to try again from —
    // which is the whole reason the raw material is stored at all (§10.1).
    let source = services
        .store
        .keep_document(DocumentToKeep {
            id: SourceId::new_random(),
            owner: principal.owner,
            broker: broker.code().to_owned(),
            format: format.code().to_owned(),
            parser_version: parser_version.clone(),
            document_hash: raw_hash.clone(),
            body: bytes.to_vec(),
        })
        .await?;
    let mut rows = submit_rows(
        services,
        principal,
        source,
        &parser_version,
        &raw_hash,
        &report,
    )
    .await?;

    for unsupported in &report.unsupported {
        rows.push(DocumentRowVerdict {
            row: unsupported.locator.row,
            verdict: Verdict::Unsupported {
                reason: unsupported.reason.code().to_owned(),
            },
        });
    }
    rows.sort_by_key(|row| row.row);

    if let Some(account) = account {
        append_control_assertions(
            services,
            AssertionOrigin {
                owner: principal.owner,
                account,
                source,
                parser_version: parser_version.clone(),
            },
            &raw_hash,
            &report,
        )
        .await?;
    }

    Ok(UploadedDocument {
        document_hash: raw_hash.as_str().to_owned(),
        source,
        broker,
        format,
        parser_version,
        period: report.period,
        rows,
    })
}
/// Re-parses the document the system kept, or the one the caller supplied.
///
/// The bytes are optional because every upload is stored: an agent that wants a
/// document parsed again names it by its hash and holds none of the owner's
/// data, which is the arrangement the design requires of it. Supplying them
/// remains possible, and their hash is still checked, for documents uploaded
/// before the system began storing sources — those recorded facts and kept no
/// body, so a reparse of one has nothing to read. That fallback closes behind
/// itself: parsing the supplied bytes stores them, and the next reparse of the
/// same document needs none.
pub async fn reparse_report(
    services: &AppServices,
    principal: &Principal,
    document_hash: &str,
    bytes: Option<&[u8]>,
    directory: &Directory,
    account: Option<AccountId>,
) -> Result<UploadedDocument, AppError> {
    // The scope is checked before the store is touched, and not only inside
    // `upload_report`: a caller who may not submit must not learn from the
    // refusal whether a document with this hash exists (§14).
    require_submit(principal)?;
    let Some(requested) = RawHash::parse(document_hash) else {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "SHA-256 of the source document".into(),
            actual: document_hash.to_owned(),
        });
    };
    if let Some(bytes) = bytes {
        let actual = hex_hash(bytes);
        if actual != requested {
            return Err(AppError::Invalid {
                field: "document".into(),
                expected: "source with the specified SHA-256".into(),
                actual: actual.as_str().to_owned(),
            });
        }
        return upload_report(services, principal, bytes, directory, account).await;
    }
    let stored = services
        .store
        .load_document_body(principal.owner, requested.clone())
        .await?;
    let Some(body) = stored else {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "a document the system kept, or its bytes in the request body".into(),
            actual: "nothing stored under this hash, and no bytes sent: the document was \
                     uploaded before the system began storing sources, or belongs to \
                     another owner"
                .into(),
        });
    };
    upload_report(services, principal, &body, directory, account).await
}

/// The permission both entry points need.
///
/// One function rather than the same check written twice: a reparse that
/// forgot it would let a read-only token drive an ingestion.
fn require_submit(principal: &Principal) -> Result<(), AppError> {
    if principal.scope.may_submit() {
        return Ok(());
    }
    Err(AppError::Invalid {
        field: "scope".into(),
        expected: "permission to submit transactions".into(),
        actual: principal.scope.code().to_owned(),
    })
}

fn detect_parser(workbook: &Workbook) -> Result<&'static dyn ReportParser, AppError> {
    // The list is fixed and exhaustive: adding a broker requires an explicit
    // change to the selection point, not a silent fallback parser.
    static TINKOFF: TinkoffParser = TinkoffParser;
    static FINAM: FinamParser = FinamParser;
    let mut found: Option<&'static dyn ReportParser> = None;
    for parser in [&TINKOFF as &dyn ReportParser, &FINAM as &dyn ReportParser] {
        if !parser.recognises(workbook) {
            continue;
        }
        if let Some(first) = found {
            return Err(AppError::Invalid {
                field: "document".into(),
                expected: "workbook unambiguously identified by a single parser".into(),
                actual: format!("{} and {}", first.broker().code(), parser.broker().code()),
            });
        }
        found = Some(parser);
    }
    found.ok_or_else(|| AppError::Invalid {
        field: "document".into(),
        expected: "supported T-Investments or Finam report".into(),
        actual: "workbook not recognised".into(),
    })
}

async fn submit_rows(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    parser_version: &ParserVersion,
    raw_hash: &RawHash,
    report: &ParsedReport,
) -> Result<Vec<DocumentRowVerdict>, AppError> {
    let mut rows = Vec::with_capacity(report.rows.len());
    let document_hash = raw_hash.as_str();
    for located in &report.rows {
        let ParsedRow::Operation(operation) = &located.outcome else {
            if let ParsedRow::Rejected(rejection) = &located.outcome {
                rows.push(DocumentRowVerdict {
                    row: located.locator.row,
                    verdict: Verdict::Rejected {
                        rejection: rejection.clone(),
                    },
                });
            }
            continue;
        };
        let mut normalized = match normalize(
            operation,
            NormalizationContext {
                owner: principal.owner,
                source,
            },
        ) {
            Ok(normalized) => normalized,
            Err(rejection) => {
                rows.push(DocumentRowVerdict {
                    row: located.locator.row,
                    verdict: Verdict::Rejected { rejection },
                });
                continue;
            }
        };
        let mut provenance = Provenance::new(source, raw_hash.clone(), parser_version.clone());
        if let Some(operation_id) = operation.source_operation_id.as_deref() {
            provenance = provenance.with_source_operation_id(operation_id);
        }
        normalized.event.provenance = provenance;
        normalized.event.idempotency_key = Some(format!(
            "report:{document_hash}:row:{}",
            located.locator.row
        ));
        // Shape, persistence and verdict are the shared ingestion foundation: a local copy here
        // would silently diverge from operations (`scenarios/ingest.rs`).
        let verdict =
            crate::scenarios::ingest::record_candidate(services, normalized.event, "operation")
                .await?;
        rows.push(DocumentRowVerdict {
            row: located.locator.row,
            verdict,
        });
    }
    Ok(rows)
}

/// Where the assertions came from: owner, account and parsing provenance.
///
/// A dedicated type rather than five consecutive parameters: arguments with the same meaning,
/// listed in sequence, are eventually swapped — and the account
/// silently ends up under the wrong owner because both fields have the same type.
struct AssertionOrigin {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
    parser_version: ParserVersion,
}

async fn append_control_assertions(
    services: &AppServices,
    origin: AssertionOrigin,
    raw_hash: &RawHash,
    report: &ParsedReport,
) -> Result<(), AppError> {
    let AssertionOrigin {
        owner,
        account,
        source,
        parser_version,
    } = origin;
    let Some(period) = report.period else {
        return Err(AppError::Invalid {
            field: "period".into(),
            expected: "reporting period".into(),
            actual: "not specified".into(),
        });
    };
    let document_hash = raw_hash.as_str();
    let provenance = Provenance::new(source, raw_hash.clone(), parser_version);
    let events: Vec<Event> = report
        .sections
        .claims()
        .into_iter()
        .enumerate()
        .map(|(sequence, claim)| Event {
            id: EventId::new_random(),
            schema_version: SCHEMA_VERSION,
            owner,
            account,
            kind: iaam_core::event::kind::EventKind::ControlAssertion { period, claim },
            dates: EventDates::empty(),
            order: EffectiveOrder::new(period.to, sequence as u32),
            legs: Vec::new(),
            provenance: provenance.clone(),
            relation: Relation::None,
            confidence: Confidence::Known,
            idempotency_key: Some(format!("report:{document_hash}:control:{sequence}")),
        })
        .collect();
    if !events.is_empty() {
        let _ = crate::scenarios::ingest::append_checked(services, events, IdentityScope::Source)
            .await?;
    }
    Ok(())
}

/// The document's identity.
///
/// The return type is [`RawHash`] rather than a string because the digest of a
/// SHA-256 is always a valid one: handing back a string would oblige every
/// caller to re-parse it and to write an arm for a failure that cannot occur.
fn hex_hash(bytes: &[u8]) -> RawHash {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    RawHash::parse(&hex).expect("a SHA-256 digest is 64 hexadecimal characters")
}

#[cfg(test)]
mod tests {
    use super::hex_hash;

    #[test]
    fn document_identity_is_a_stable_sha256_hex() {
        assert_eq!(
            hex_hash(b"iaam").as_str(),
            "9b04d18aa56c16fde8f892a4ca34726b65abd2e2f2c9e9e03b475775f6e345e2"
        );
    }
}
