//! Reconciliation of the operation type dictionary against the published contract (§14).
//!
//! Answers one question: which codes the broker has declared but the dictionary
//! does not know. The reconciliation cannot and does not attempt to classify them —
//! the contract lists the codes but does not say what they map to
//! in our system. The meaning of a new code is approved by the owner.

use iaam_broker::tinkoff::contract::{operation_types_request, parse_operation_types};
use iaam_store::documents::BrokerCode;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{BrokerDictionary, OutboundHttp};

/// Reconciliation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryGap {
    /// Number of codes declared by the contract.
    pub declared: usize,
    /// Number of them already known to the dictionary.
    pub known: usize,
    /// Codes missing from the dictionary, by name.
    ///
    /// Specifically by name, rather than as a count: «three codes have appeared» does not allow
    /// either a decision to be made, or even determining whether these are the same three as
    /// last time.
    pub missing: Vec<String>,
}

/// Reconcile the channel dictionary with the contract.
///
/// Contract unavailability is a failure, not an empty list of discrepancies:
/// «checked, everything is present» and «could not check» must remain distinct,
/// otherwise the very first network failure looks as if all is well.
pub async fn compare_with_contract(
    http: &dyn OutboundHttp,
    dictionary: &dyn BrokerDictionary,
    broker: &BrokerCode,
) -> Result<DictionaryGap, AppError> {
    let response = http.send(operation_types_request()).await?;
    if !(200..=299).contains(&response.status) {
        return Err(AppError::Invalid {
            field: "contract".to_owned(),
            expected: "a successful response when fetching the contract".to_owned(),
            actual: response.status.to_string(),
        });
    }
    let body = String::from_utf8(response.body).map_err(|_| AppError::Invalid {
        field: "contract".to_owned(),
        expected: "contract text".to_owned(),
        actual: "not UTF-8".to_owned(),
    })?;
    let declared = parse_operation_types(&body).map_err(|error| AppError::Invalid {
        field: "contract".to_owned(),
        expected: "an OperationType list".to_owned(),
        actual: error.to_string(),
    })?;

    let known = dictionary.operation_kinds(broker).await?;

    let mut missing: Vec<String> = declared
        .iter()
        .filter(|source_kind| !known.contains_key(*source_kind))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();

    Ok(DictionaryGap {
        declared: declared.len(),
        known: declared.len() - missing.len(),
        missing,
    })
}

/// Reconcile the dictionary using the application's assembled dependencies.
///
/// The server calls this façade, while the scenario above takes the ports separately:
/// reconciliation needs two out of ten, and requiring the entire set to be assembled would mean
/// requiring a configured journal in a test merely to read the reference data.
pub async fn compare_with_contract_using_services(
    services: &AppServices,
    broker: &BrokerCode,
) -> Result<DictionaryGap, AppError> {
    compare_with_contract(
        services.http.as_ref(),
        services.broker_dictionary.as_ref(),
        broker,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use iaam_http::HttpRequest;

    use super::*;
    use crate::ports::{BrokerDictionary, OutboundHttp, OutboundResponse};

    const CONTRACT: &str = r"
enum OperationType {
  OPERATION_TYPE_INPUT = 1;
  OPERATION_TYPE_COUPON = 23;
  OPERATION_TYPE_BOND_REPAYMENT = 10;
}
";

    struct FixedContract {
        body: Vec<u8>,
        status: u16,
    }

    #[async_trait]
    impl OutboundHttp for FixedContract {
        async fn send(&self, _request: HttpRequest) -> Result<OutboundResponse, AppError> {
            Ok(OutboundResponse {
                status: self.status,
                raw_hash: "contract".to_owned(),
                body: self.body.clone(),
            })
        }
    }

    struct KnownCodes(BTreeMap<String, String>);

    #[async_trait]
    impl BrokerDictionary for KnownCodes {
        async fn operation_kinds(
            &self,
            _broker: &BrokerCode,
        ) -> Result<BTreeMap<String, String>, AppError> {
            Ok(self.0.clone())
        }
    }

    fn parts(
        contract: &str,
        status: u16,
        known: &[(&str, &str)],
    ) -> (Arc<FixedContract>, Arc<KnownCodes>) {
        (
            Arc::new(FixedContract {
                body: contract.as_bytes().to_vec(),
                status,
            }),
            Arc::new(KnownCodes(
                known
                    .iter()
                    .map(|(code, kind)| ((*code).to_owned(), (*kind).to_owned()))
                    .collect(),
            )),
        )
    }

    fn tinkoff() -> BrokerCode {
        BrokerCode::parse("tinkoff").expect("broker code")
    }

    #[tokio::test]
    async fn the_codes_absent_from_the_dictionary_are_named_one_by_one() {
        let (http, known) = parts(CONTRACT, 200, &[("OPERATION_TYPE_INPUT", "deposit")]);
        let gap = compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
            .await
            .expect("reconciliation succeeded");
        assert_eq!(gap.declared, 3);
        assert_eq!(gap.known, 1);
        assert_eq!(
            gap.missing,
            ["OPERATION_TYPE_BOND_REPAYMENT", "OPERATION_TYPE_COUPON"],
            "the discrepancy must name the codes, not their count"
        );
    }

    #[tokio::test]
    async fn a_full_dictionary_leaves_nothing_missing() {
        let (http, known) = parts(
            CONTRACT,
            200,
            &[
                ("OPERATION_TYPE_INPUT", "deposit"),
                ("OPERATION_TYPE_COUPON", "coupon"),
                ("OPERATION_TYPE_BOND_REPAYMENT", "bond_amortisation"),
            ],
        );
        let gap = compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
            .await
            .expect("reconciliation succeeded");
        assert!(gap.missing.is_empty(), "{:?}", gap.missing);
        assert_eq!(gap.known, gap.declared);
    }

    /// «Checked, everything is present» and «could not check» must
    /// remain distinct: otherwise the very first network failure looks as if all is well,
    /// and the dictionary quietly falls behind the contract.
    #[tokio::test]
    async fn an_unavailable_contract_is_a_refusal_not_an_empty_gap() {
        let (http, known) = parts("", 503, &[]);
        assert!(
            compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
                .await
                .is_err()
        );
    }

    /// A response arrived, but it contains no list — also a failure: the page
    /// «repository has moved» is returned with code 200.
    #[tokio::test]
    async fn a_body_without_the_enum_is_a_refusal() {
        let (http, known) = parts("moved; look elsewhere", 200, &[]);
        assert!(
            compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
                .await
                .is_err()
        );
    }
}
