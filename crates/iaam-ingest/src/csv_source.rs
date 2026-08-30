//! CSV parsing (§10.1).
//!
//! A line is a parsing unit: an unrecognized line receives a verdict, and
//! document parsing continues. Account and instrument are specified by
//! human-readable names and resolved through the reference directory: UUIDs
//! in a file filled in by a human are a way to guarantee
//! errors.
//!
//! Amounts are recorded as decimal numbers. A number with greater precision,
//! than the currency's minimum unit is **rejected**, not rounded:
//! rounding input data is a silent alteration of the fact.

use std::collections::{BTreeMap, BTreeSet};

use iaam_core::event::kind::FeeOrigin;
use iaam_core::ids::{AccountId, CustodyId, InstrumentId};
use iaam_core::instrument::AliasInterval;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use iaam_core::valuation::PriceQuality;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::Date;
use time::macros::format_description;

use crate::operation::{OperationDates, OperationKind, SubmittedOperation, to_minor_units};
use crate::verdict::Rejection;

/// Name directory. Populated by a wrapper from account and instrument tables.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub accounts: AccountNames,
    pub custodies: CustodyNames,
    pub instruments: InstrumentAliases,
    /// Default custody location for an account without a specified custodian.
    pub default_custody: Option<CustodyId>,
}

/// Account names, preserving all matches.
pub type AccountNames = BTreeMap<String, Vec<AccountId>>;

/// Custody location names, preserving all matches.
pub type CustodyNames = BTreeMap<String, Vec<CustodyId>>;

/// Instrument aliases with their validity intervals.
///
/// A flat “code → identifier” map is incorrect: one ISIN
/// belongs to different issues in different years, while one issue changes its
/// ISIN through a corporate action during its lifetime (§4.7). Resolution is performed
/// for the row date, not “today”.
pub type InstrumentAliases = BTreeMap<String, Vec<(String, AliasInterval, InstrumentId)>>;

/// Instrument by code on the row date.
pub fn resolve_instrument(
    aliases: &InstrumentAliases,
    code: &str,
    on: Date,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        });
    };
    let matching: BTreeSet<InstrumentId> = candidates
        .iter()
        .filter(|(_, interval, _)| interval.covers(on))
        .map(|(_, _, id)| *id)
        .collect();
    match matching.len() {
        1 => Ok(*matching.first().expect("non-empty set")),
        // The code is known, but not for this date. Use a separate message rather than a generic
        // rejection: this indicates a corrupted document date, not a new
        // instrument, and the person investigating must see the difference.
        0 => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "code effective on the operation date".into(),
            actual: code.to_owned(),
        }),
        // Multiple namespaces may contain the same code. This is
        // safe if they refer to the same issue; different issues
        // require an explicit correction to the input data.
        _ => {
            let namespaces: BTreeSet<&str> = candidates
                .iter()
                .filter(|(_, interval, _)| interval.covers(on))
                .map(|(namespace, _, _)| namespace.as_str())
                .collect();
            Err(Rejection {
                field: "instrument".to_owned(),
                expected: "one instrument among the active namespaces".into(),
                actual: format!(
                    "{code}: ambiguity between namespaces {}",
                    namespaces.into_iter().collect::<Vec<_>>().join(", ")
                ),
            })
        }
    }
}

/// Instrument for a code from the named namespace on the given date.
///
/// An explicit namespace does not fall back to another namespace: a report
/// that names a ticker must not accidentally select `broker_code`.
pub(crate) fn resolve_instrument_in_namespace(
    aliases: &InstrumentAliases,
    namespace: &str,
    code: &str,
    on: Date,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        });
    };
    if !candidates
        .iter()
        .any(|(candidate_namespace, _, _)| candidate_namespace == namespace)
    {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        });
    }
    let matching: BTreeSet<InstrumentId> = candidates
        .iter()
        .filter(|(candidate_namespace, interval, _)| {
            candidate_namespace == namespace && interval.covers(on)
        })
        .map(|(_, _, id)| *id)
        .collect();
    match matching.len() {
        1 => Ok(*matching.first().expect("non-empty set")),
        0 => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "code effective on the operation date".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "one instrument among the active namespaces".into(),
            actual: format!("{code}: ambiguity in namespace {namespace}"),
        }),
    }
}

/// Instrument for a code from the named namespace without a snapshot date.
pub(crate) fn resolve_instrument_in_namespace_without_date(
    aliases: &InstrumentAliases,
    namespace: &str,
    code: &str,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        });
    };
    let namespaced: Vec<_> = candidates
        .iter()
        .filter(|(candidate_namespace, _, _)| candidate_namespace == namespace)
        .collect();
    match namespaced.as_slice() {
        [(_, _, instrument)] => Ok(*instrument),
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "report snapshot date for selecting the instrument code".into(),
            actual: code.to_owned(),
        }),
    }
}

/// Instrument for a snapshot without a date: only a single alias is allowed.
///
/// If the report did not specify the snapshot date, multiple historical variants
/// cannot be resolved by guessing. A single record preserves the previous
/// unambiguous scenario; a history of two or more records is rejected.
pub fn resolve_instrument_without_date(
    aliases: &InstrumentAliases,
    code: &str,
) -> Result<InstrumentId, Rejection> {
    let Some(candidates) = aliases.get(code) else {
        return Err(Rejection {
            field: "instrument".to_owned(),
            expected: "instrument code from the reference directory".into(),
            actual: code.to_owned(),
        });
    };
    match candidates.as_slice() {
        [(_, _, instrument)] => Ok(*instrument),
        [] => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "code effective on the operation date".into(),
            actual: code.to_owned(),
        }),
        _ => Err(Rejection {
            field: "instrument".to_owned(),
            expected: "report snapshot date for selecting the instrument code".into(),
            actual: code.to_owned(),
        }),
    }
}

/// One file row in raw form.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub date: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub account: String,
    #[serde(default)]
    pub counterparty_account: Option<String>,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub custody: Option<String>,
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub fee: Option<String>,
    #[serde(default)]
    pub accrued_interest: Option<String>,
    pub currency: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// The result of parsing one row.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRow {
    Operation(Box<SubmittedOperation>),
    Rejected(Rejection),
}

/// Parse the entire document. Returns one item per row, including
/// rejected rows: the row number is the result index plus one.
#[must_use]
pub fn parse(content: &str, directory: &Directory) -> Vec<ParsedRow> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());
    let mut parsed = Vec::new();
    for record in reader.deserialize::<Row>() {
        parsed.push(match record {
            Ok(row) => match row_to_operation(&row, directory) {
                Ok(operation) => ParsedRow::Operation(Box::new(operation)),
                Err(rejection) => ParsedRow::Rejected(rejection),
            },
            Err(error) => ParsedRow::Rejected(Rejection {
                field: "row".into(),
                expected: "file header row format".into(),
                actual: error.to_string(),
            }),
        });
    }
    parsed
}

fn row_to_operation(row: &Row, directory: &Directory) -> Result<SubmittedOperation, Rejection> {
    let date = parse_date(&row.date)?;
    let currency = parse_currency(&row.currency)?;
    let account = lookup(&directory.accounts, &row.account, "account", "account")?;
    let kind = build_kind(row, directory, currency, date)?;

    Ok(SubmittedOperation {
        account,
        kind,
        dates: OperationDates {
            trade: Some(date),
            cash_posted: Some(date),
            ..OperationDates::default()
        },
        idempotency_key: row.idempotency_key.clone(),
        source_operation_id: None,
    })
}

fn build_kind(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
    date: Date,
) -> Result<OperationKind, Rejection> {
    match row.kind.as_str() {
        "deposit" => Ok(OperationKind::Deposit {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "withdrawal" => Ok(OperationKind::Withdrawal {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "transfer" => Ok(OperationKind::Transfer {
            to: lookup(
                &directory.accounts,
                row.counterparty_account.as_deref().unwrap_or_default(),
                "counterparty_account",
                "account",
            )?,
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
        }),
        "buy" | "sell" => build_trade(row, directory, currency, date),
        "income" => Ok(OperationKind::Income {
            instrument: match row.instrument.as_deref() {
                None | Some("") => None,
                Some(symbol) => Some(resolve_instrument(&directory.instruments, symbol, date)?),
            },
            gross_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
            // The CSV row has no income-type column: the source
            // did not specify one, and supplying a type would be fabrication (§4.9).
            kind: None,
        }),
        "fee" => Ok(OperationKind::Fee {
            amount_minor: minor(row.amount.as_deref(), "amount", currency)?,
            currency,
            origin: FeeOrigin::Other,
        }),
        "valuation" => Ok(OperationKind::Valuation {
            instrument: resolve_instrument(
                &directory.instruments,
                row.instrument.as_deref().unwrap_or_default(),
                date,
            )?,
            price: Dec::new(decimal(row.amount.as_deref(), "amount")?),
            currency,
            // The owner's quoted price is not executable (§5.4).
            quality: PriceQuality::OwnerEstimate,
        }),
        other => Err(Rejection {
            field: "type".into(),
            expected: "deposit, withdrawal, transfer, buy, sell, income, fee or valuation".into(),
            actual: other.to_owned(),
        }),
    }
}

/// Storage location: a named one is validated against the directory, an unnamed one
/// is taken from the default.
///
/// Extracted into a separate function **for testability, not readability**.
/// An empty string cannot be obtained from inside `parse`: CSV parsing returns
/// `None` for an empty cell, not `Some("")`. The branch checking for emptiness
/// would therefore be unreachable, and the mutation shield would call it
/// equivalent. An unreachable check is one whose behavior is unknown;
/// a separate function can be called directly, including with an empty string.
fn resolve_custody(name: Option<&str>, directory: &Directory) -> Result<CustodyId, Rejection> {
    match name {
        Some(name) if !name.is_empty() => resolve_named_custody(name, directory, "custody"),
        _ => directory.default_custody.ok_or_else(|| Rejection {
            field: "custody".into(),
            expected: "known storage location or default value".into(),
            actual: "not specified".into(),
        }),
    }
}
pub(crate) fn resolve_named_account(
    name: &str,
    directory: &Directory,
    field: &'static str,
) -> Result<AccountId, Rejection> {
    lookup(&directory.accounts, name, field, "account")
}

pub(crate) fn resolve_named_custody(
    name: &str,
    directory: &Directory,
    field: &'static str,
) -> Result<CustodyId, Rejection> {
    lookup(&directory.custodies, name, field, "storage location")
}

fn build_trade(
    row: &Row,
    directory: &Directory,
    currency: CurrencyCode,
    date: Date,
) -> Result<OperationKind, Rejection> {
    let instrument = resolve_instrument(
        &directory.instruments,
        row.instrument.as_deref().unwrap_or_default(),
        date,
    )?;
    let custody = resolve_custody(row.custody.as_deref(), directory)?;
    let quantity = Dec::new(decimal(row.quantity.as_deref(), "quantity")?);
    let gross_minor = minor(row.amount.as_deref(), "amount", currency)?;
    let fee_minor = optional_minor(row.fee.as_deref(), "fee", currency)?;
    let accrued_interest_minor = optional_minor(
        row.accrued_interest.as_deref(),
        "accrued_interest",
        currency,
    )?;

    if row.kind == "buy" {
        Ok(OperationKind::Buy {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    } else {
        Ok(OperationKind::Sell {
            instrument,
            custody,
            quantity,
            gross_minor,
            fee_minor,
            accrued_interest_minor,
            currency,
        })
    }
}

fn lookup<T: Copy>(
    table: &BTreeMap<String, Vec<T>>,
    name: &str,
    field: &'static str,
    entity: &'static str,
) -> Result<T, Rejection> {
    let Some(candidates) = table.get(name) else {
        return Err(Rejection {
            field: field.to_owned(),
            expected: "directory name".into(),
            actual: name.to_owned(),
        });
    };
    match candidates.as_slice() {
        [single] => Ok(*single),
        [] => Err(Rejection {
            field: field.to_owned(),
            expected: "directory name".into(),
            actual: name.to_owned(),
        }),
        _ => Err(Rejection {
            field: field.to_owned(),
            expected: "unambiguous name from the directory".into(),
            actual: format!(
                "{name}: {entity} name is ambiguous: {} {entity}s",
                candidates.len()
            ),
        }),
    }
}

fn parse_date(value: &str) -> Result<Date, Rejection> {
    Date::parse(value, format_description!("[year]-[month]-[day]")).map_err(|_| Rejection {
        field: "date".into(),
        expected: "date in YYYY-MM-DD format".into(),
        actual: value.to_owned(),
    })
}

fn parse_currency(value: &str) -> Result<CurrencyCode, Rejection> {
    match value {
        "RUB" => Ok(CurrencyCode::Rub),
        "USD" => Ok(CurrencyCode::Usd),
        "EUR" => Ok(CurrencyCode::Eur),
        "CNY" => Ok(CurrencyCode::Cny),
        "XAU" => Ok(CurrencyCode::Xau),
        other => Err(Rejection {
            field: "currency".into(),
            expected: "RUB, USD, EUR, CNY or XAU".into(),
            actual: other.to_owned(),
        }),
    }
}

fn decimal(value: Option<&str>, field: &'static str) -> Result<Decimal, Rejection> {
    let raw = value.unwrap_or_default();
    raw.parse::<Decimal>().map_err(|_| Rejection {
        field: field.to_owned(),
        expected: "decimal number".into(),
        actual: raw.to_owned(),
    })
}

fn minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<i64, Rejection> {
    to_minor_units(decimal(value, field)?, currency, field)
}

fn optional_minor(
    value: Option<&str>,
    field: &'static str,
    currency: CurrencyCode,
) -> Result<Option<i64>, Rejection> {
    match value {
        None | Some("") => Ok(None),
        Some(raw) => minor(Some(raw), field, currency).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn an_instrument_named_by_code_is_resolved_on_the_row_date() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let found = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01));

        assert_eq!(found.expect("code is allowed"), instrument);
    }
    #[test]
    fn an_explicit_namespace_does_not_fall_back_to_other_namespaces() {
        let broker_instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "broker_code".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                broker_instrument,
            )],
        );

        let refused =
            resolve_instrument_in_namespace(&aliases, "ticker", "SBER", date!(2024 - 03 - 01))
                .expect_err("ticker must not search broker_code");

        assert_eq!(refused.field, "instrument");
        assert_eq!(
            refused.expected,
            "instrument code from the reference directory"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_instrument_known_in_multiple_namespaces_resolves_to_the_same_id() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
            ],
        );

        let found = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01));

        assert_eq!(
            found.expect("codes from different namespaces are allowed"),
            instrument
        );
    }

    #[test]
    fn an_instrument_code_is_rejected_when_namespaces_point_to_different_ids() {
        let ticker_instrument = InstrumentId::new_random();
        let broker_instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    ticker_instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    broker_instrument,
                ),
            ],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("the same value in different namespaces is ambiguous");

        assert_eq!(refused.field, "instrument");
        assert!(refused.actual.contains("SBER"));
        assert!(refused.actual.contains("ticker"));
        assert!(refused.actual.contains("broker_code"));
    }

    #[test]
    fn an_instrument_with_multiple_namespaces_still_requires_a_date() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
                (
                    "broker_code".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    instrument,
                ),
            ],
        );

        let refused = resolve_instrument_without_date(&aliases, "SBER")
            .expect_err("a namespace cannot be selected without a date");

        assert_eq!(
            refused.expected,
            "report snapshot date for selecting the instrument code"
        );
    }

    #[test]
    fn namespace_without_date_accepts_single_alias() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        assert_eq!(
            resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
                .expect("only one alias is allowed"),
            instrument
        );
    }

    #[test]
    fn namespace_without_date_rejects_code_from_other_namespace() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "broker_code".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused = resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
            .expect_err("a foreign namespace must not be resolved");

        assert_eq!(refused.field, "instrument");
        assert_eq!(
            refused.expected,
            "instrument code from the reference directory"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn namespace_without_date_rejects_multiple_aliases_with_reason() {
        let first = InstrumentId::new_random();
        let second = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2020 - 01 - 01),
                        valid_to: None,
                    },
                    first,
                ),
                (
                    "ticker".to_owned(),
                    AliasInterval {
                        valid_from: date!(2024 - 01 - 01),
                        valid_to: None,
                    },
                    second,
                ),
            ],
        );

        let refused = resolve_instrument_in_namespace_without_date(&aliases, "ticker", "SBER")
            .expect_err("multiple aliases require a snapshot date");

        assert_eq!(
            refused.expected,
            "report snapshot date for selecting the instrument code"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn dated_namespace_distinguishes_unknown_code_from_ambiguous_code() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused =
            resolve_instrument_in_namespace(&aliases, "ticker", "SBER", date!(2024 - 03 - 01))
                .expect_err("a code outside the interval must be rejected");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "code effective on the operation date");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn a_duplicate_custody_title_is_rejected_as_ambiguous() {
        let first = CustodyId::new_random();
        let second = CustodyId::new_random();
        let mut directory = Directory::default();
        directory
            .custodies
            .insert("НРД".into(), vec![first, second]);

        let refused = resolve_custody(Some("НРД"), &directory)
            .expect_err("the same custody name is ambiguous");

        assert_eq!(refused.field, "custody");
        assert!(refused.actual.contains("НРД"));
        assert!(refused.actual.contains("ambiguous"));
    }

    #[test]
    fn an_instrument_code_is_rejected_on_the_first_day_after_its_interval() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: Some(date!(2024 - 03 - 01)),
                },
                instrument,
            )],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("the interval has already ended");

        assert_eq!(refused.field, "instrument");
    }

    #[test]
    fn an_unknown_code_is_refused_and_not_skipped() {
        let aliases = BTreeMap::new();

        let refused =
            resolve_instrument(&aliases, "NOPE", date!(2024 - 03 - 01)).expect_err("unknown code");

        assert_eq!(refused.field, "instrument");
    }

    #[test]
    fn a_code_outside_its_interval_is_told_apart_from_an_unknown_one() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let unknown = resolve_instrument(&BTreeMap::new(), "SBER", date!(2024 - 03 - 01))
            .expect_err("code is missing");
        let too_early = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("code was not yet in effect");

        assert_ne!(
            unknown.expected, too_early.expected,
            "new security and corrupted date must sound different"
        );
    }

    #[test]
    fn an_unnamed_custody_falls_back_to_the_default_and_a_named_one_is_looked_up() {
        // Three name states: unset, set to empty, set.
        // An empty string means the same as absence: the custody was not named.
        // An empty string cannot be obtained from inside CSV parsing,
        // so the check is called directly here.
        let default = CustodyId::new_random();
        let named = CustodyId::new_random();
        let mut directory = Directory {
            default_custody: Some(default),
            ..Directory::default()
        };
        directory.custodies.insert("НРД".into(), vec![named]);

        assert_eq!(resolve_custody(None, &directory).unwrap(), default);
        assert_eq!(resolve_custody(Some(""), &directory).unwrap(), default);
        assert_eq!(resolve_custody(Some("НРД"), &directory).unwrap(), named);
        assert_eq!(
            resolve_custody(Some("Неизвестный"), &directory)
                .unwrap_err()
                .field,
            "custody"
        );
    }

    #[test]
    fn without_a_default_an_unnamed_custody_is_a_refusal_not_a_guess() {
        let directory = Directory::default();
        let rejection = resolve_custody(None, &directory).unwrap_err();
        assert_eq!(rejection.field, "custody");
        assert_eq!(rejection.actual, "not specified");
    }
    #[test]
    fn a_known_code_outside_its_interval_reports_a_date_specific_rejection() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2025 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        let refused = resolve_instrument(&aliases, "SBER", date!(2024 - 03 - 01))
            .expect_err("code is known but not effective on the date");

        assert_eq!(refused.field, "instrument");
        assert_eq!(
            refused.expected, "code effective on the operation date",
            "a code known to the reference data must differ from an unknown code"
        );
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_instrument_without_date_returns_the_only_candidate() {
        let instrument = InstrumentId::new_random();
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "SBER".to_owned(),
            vec![(
                "ticker".to_owned(),
                AliasInterval {
                    valid_from: date!(2020 - 01 - 01),
                    valid_to: None,
                },
                instrument,
            )],
        );

        assert_eq!(
            resolve_instrument_without_date(&aliases, "SBER").expect("the only code"),
            instrument
        );
    }

    #[test]
    fn an_empty_instrument_history_without_date_has_the_date_specific_rejection() {
        let mut aliases = BTreeMap::new();
        aliases.insert("SBER".to_owned(), Vec::new());

        let refused = resolve_instrument_without_date(&aliases, "SBER")
            .expect_err("empty instrument history");

        assert_eq!(refused.field, "instrument");
        assert_eq!(refused.expected, "code effective on the operation date");
        assert_eq!(refused.actual, "SBER");
    }

    #[test]
    fn an_empty_custody_history_is_rejected_as_an_unknown_name() {
        let mut directory = Directory::default();
        directory.custodies.insert("НРД".into(), Vec::new());

        let refused =
            resolve_named_custody("НРД", &directory, "custody").expect_err("empty custody history");

        assert_eq!(refused.field, "custody");
        assert_eq!(refused.expected, "directory name");
        assert_eq!(refused.actual, "НРД");
    }
}
