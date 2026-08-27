//! Загрузка и повторный разбор отчётов.
//!
//! Отчётный парсер остаётся в `iaam-ingest`; этот сценарий только выбирает
//! его, записывает разобранные операции и возвращает исход по каждой строке.

use iaam_core::dates::{EffectiveOrder, EventDates};
use iaam_core::event::provenance::{ParserVersion, Provenance, RawHash};
use iaam_core::event::{Confidence, Event, Relation, SCHEMA_VERSION};
use iaam_core::ids::{AccountId, EventId, OwnerId, SourceId};
use iaam_ingest::Verdict;
use iaam_ingest::csv_source::{Directory, ParsedRow};
use iaam_ingest::operation::{NormalizationContext, normalize};
use iaam_ingest::report::finam::FinamParser;
use iaam_ingest::report::tinkoff::TinkoffParser;
use iaam_ingest::report::workbook::Workbook;
use iaam_ingest::report::{Broker, ParsedReport, ReportFormat as ParsedFormat, ReportParser};
use sha2::{Digest, Sha256};

use crate::AppServices;
use crate::error::AppError;
use crate::ports::Principal;
/// Один исход загрузки с номером строки исходного листа.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRowVerdict {
    pub row: u64,
    pub verdict: Verdict,
}

/// Результат разбора документа.
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

/// Разбирает и записывает отчёт, не отменяя документ из-за одной плохой строки.
pub async fn upload_report(
    services: &AppServices,
    principal: &Principal,
    bytes: &[u8],
    directory: &Directory,
    account: Option<AccountId>,
) -> Result<UploadedDocument, AppError> {
    if !principal.scope.may_submit() {
        return Err(AppError::Invalid {
            field: "scope".into(),
            expected: "право отправки операций".into(),
            actual: principal.scope.code().to_owned(),
        });
    }

    let workbook = Workbook::open(bytes).map_err(|error| AppError::Invalid {
        field: "document".into(),
        expected: "поддерживаемая книга отчёта".into(),
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
            expected: "идентификатор счёта для контрольных секций".into(),
            actual: "не указан".into(),
        });
    }
    let source = SourceId::new_random();
    let document_hash = hex_hash(bytes);
    let mut rows = submit_rows(
        services,
        principal,
        source,
        &parser_version,
        &document_hash,
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
            &document_hash,
            &report,
        )
        .await?;
    }

    Ok(UploadedDocument {
        document_hash,
        source,
        broker,
        format,
        parser_version,
        period: report.period,
        rows,
    })
}
/// Повторно разбирает переданный исходник и требует совпадения его хеша.
pub async fn reparse_report(
    services: &AppServices,
    principal: &Principal,
    document_hash: &str,
    bytes: &[u8],
    directory: &Directory,
    account: Option<AccountId>,
) -> Result<UploadedDocument, AppError> {
    let actual = hex_hash(bytes);
    if actual != document_hash.to_ascii_lowercase() {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "исходник с указанным SHA-256".into(),
            actual,
        });
    }
    upload_report(services, principal, bytes, directory, account).await
}

fn detect_parser(workbook: &Workbook) -> Result<&'static dyn ReportParser, AppError> {
    // Список фиксирован и исчерпываем: добавление брокера требует явного
    // изменения точки выбора, а не молчаливого fallback-парсера.
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
                expected: "книга, однозначно опознанная одним парсером".into(),
                actual: format!("{} и {}", first.broker().code(), parser.broker().code()),
            });
        }
        found = Some(parser);
    }
    found.ok_or_else(|| AppError::Invalid {
        field: "document".into(),
        expected: "поддерживаемый отчёт Т-Инвестиций или Финама".into(),
        actual: "книга не опознана".into(),
    })
}

async fn submit_rows(
    services: &AppServices,
    principal: &Principal,
    source: SourceId,
    parser_version: &ParserVersion,
    document_hash: &str,
    report: &ParsedReport,
) -> Result<Vec<DocumentRowVerdict>, AppError> {
    let mut rows = Vec::with_capacity(report.rows.len());
    let Some(raw_hash) = RawHash::parse(document_hash) else {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "SHA-256".into(),
            actual: document_hash.to_owned(),
        });
    };
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
        // Форма, запись и вердикт — общий низ приёмки: своя копия здесь
        // разошлась бы с операциями молча (`scenarios/ingest.rs`).
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

/// Откуда взялись утверждения: владелец, счёт и происхождение разбора.
///
/// Отдельный тип, а не пять параметров подряд: аргументы одного смысла,
/// перечисленные в строку, рано или поздно меняются местами — и счёт
/// уезжает в чужого владельца молча, потому что оба поля одного типа.
struct AssertionOrigin {
    owner: OwnerId,
    account: AccountId,
    source: SourceId,
    parser_version: ParserVersion,
}

async fn append_control_assertions(
    services: &AppServices,
    origin: AssertionOrigin,
    document_hash: &str,
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
            expected: "период отчёта".into(),
            actual: "не указан".into(),
        });
    };
    let Some(raw_hash) = RawHash::parse(document_hash) else {
        return Err(AppError::Invalid {
            field: "document".into(),
            expected: "SHA-256".into(),
            actual: document_hash.to_owned(),
        });
    };
    let provenance = Provenance::new(source, raw_hash, parser_version);
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
        let _ = services.store.append_events(events).await?;
    }
    Ok(())
}

fn hex_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::hex_hash;

    #[test]
    fn document_identity_is_a_stable_sha256_hex() {
        assert_eq!(
            hex_hash(b"iaam"),
            "9b04d18aa56c16fde8f892a4ca34726b65abd2e2f2c9e9e03b475775f6e345e2"
        );
    }
}
