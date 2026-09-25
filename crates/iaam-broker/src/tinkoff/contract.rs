//! Published T-Invest contract: operation kinds (§14).
//!
//! The kind dictionary lives in data, and there is something to compare it
//! against: T-Invest publishes `operations.proto`, where the `OperationType`
//! list is present as source text. The comparison answers one question: which
//! codes appeared since the previous version.
//!
//! **The comparison cannot and does not try to classify a new code.**
//! The contract names codes but does not say what they become for us:
//! `OPERATION_TYPE_OVERNIGHT` may be income or a fee; the owner decides, not
//! the protocol text. A task that guessed would record an unapproved meaning
//! in the dictionary.

use iaam_http::{Destination, HttpRequest};

/// Location of the contract.
///
/// The path is a constant beside parsing because it is part of the same answer
/// to “where did this come from?” as the list itself.
pub const OPERATIONS_CONTRACT_PATH: &str =
    "/RussianInvestments/investAPI/main/src/docs/contracts/operations.proto";

/// Name for the dictionary source in the provenance record.
#[must_use]
pub fn contract_dictionary_name() -> String {
    format!("t-invest:{OPERATIONS_CONTRACT_PATH}")
}

/// Request the contract.
#[must_use]
pub fn operation_types_request() -> HttpRequest {
    HttpRequest::get(Destination::TinvestContract, OPERATIONS_CONTRACT_PATH)
}

/// Why the list could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("OperationType list is absent from the contract")]
    EnumMissing,
    #[error("OperationType list is not closed with a brace")]
    EnumUnterminated,
    /// The list was found but is empty.
    ///
    /// A separate reason rather than an empty list: an empty list reads as
    /// “there are no discrepancies”, making a parse refusal look like a
    /// successful comparison.
    #[error("OperationType list is empty")]
    EnumEmpty,
}

/// Operation-kind codes from the contract text.
///
/// Parsing is intentionally coarse: we need member names, not the whole
/// protocol. Pulling in a protobuf parser for this would add a dependency
/// that knows incomparably more than needed.
pub fn parse_operation_types(contract: &str) -> Result<Vec<String>, ContractError> {
    let start = contract
        .find("enum OperationType")
        .ok_or(ContractError::EnumMissing)?;
    let body = &contract[start..];
    let open = body.find('{').ok_or(ContractError::EnumUnterminated)?;
    let close = body.find('}').ok_or(ContractError::EnumUnterminated)?;
    if close < open {
        return Err(ContractError::EnumUnterminated);
    }
    let mut codes = Vec::new();
    for line in body[open + 1..close].lines() {
        // Drop comments entirely: they can contain the same member names, and
        // treating them as declarations would create a duplicate.
        let line = line.split("//").next().unwrap_or("").trim();
        let Some((name, _)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("OPERATION_TYPE_") {
            codes.push(name.to_owned());
        }
    }
    if codes.is_empty() {
        return Err(ContractError::EnumEmpty);
    }
    Ok(codes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"
enum OperationType {
  OPERATION_TYPE_UNSPECIFIED = 0;
  // OPERATION_TYPE_FROM_A_COMMENT = 99;
  OPERATION_TYPE_INPUT = 1; // cash deposit
  OPERATION_TYPE_BOND_REPAYMENT_FULL = 6;
}
";

    #[test]
    fn the_members_are_read_in_the_order_the_contract_lists_them() {
        assert_eq!(
            parse_operation_types(SAMPLE).expect("OperationType list was read"),
            [
                "OPERATION_TYPE_UNSPECIFIED",
                "OPERATION_TYPE_INPUT",
                "OPERATION_TYPE_BOND_REPAYMENT_FULL",
            ]
        );
    }

    /// A member name in a comment is not a declaration. Treating it as one
    /// would add a code absent from the broker.
    #[test]
    fn a_name_inside_a_comment_is_not_a_member() {
        let codes = parse_operation_types(SAMPLE).expect("OperationType list was read");
        assert!(!codes.iter().any(|code| code.contains("FROM_A_COMMENT")));
    }

    #[test]
    fn a_contract_without_the_enum_is_a_refusal() {
        assert_eq!(
            parse_operation_types("enum SomethingElse { A = 1; }"),
            Err(ContractError::EnumMissing)
        );
    }

    /// An empty list is a refusal, not an empty answer: “no discrepancies”
    /// and “could not read” must remain distinct.
    #[test]
    fn an_empty_enum_is_a_refusal_not_an_empty_answer() {
        assert_eq!(
            parse_operation_types("enum OperationType {\n}\n"),
            Err(ContractError::EnumEmpty)
        );
    }

    #[test]
    fn an_unterminated_enum_is_a_refusal() {
        assert_eq!(
            parse_operation_types("enum OperationType { OPERATION_TYPE_INPUT = 1;"),
            Err(ContractError::EnumUnterminated)
        );
    }

    #[test]
    fn the_request_goes_to_the_contract_host_not_to_the_gateway() {
        let request = operation_types_request();
        assert!(
            request.url().contains("raw.githubusercontent.com"),
            "{}",
            request.url()
        );
        assert!(
            request.url().contains("operations.proto"),
            "{}",
            request.url()
        );
    }

    /// The contract host is paced by the gateway like any destination: a
    /// host with no budget row would be refused before anything was sent.
    #[tokio::test]
    async fn the_request_is_paced_by_the_gateway_one_a_second() {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        use iaam_http::gateway::{BUDGETS, Clock, Sleeper, Transport};
        use iaam_http::{Gateway, HttpError, HttpResponse};

        struct FakeTime(Mutex<Instant>, Mutex<Vec<Duration>>);
        impl Clock for FakeTime {
            fn now(&self) -> Instant {
                *self.0.lock().expect("clock")
            }
        }
        impl Sleeper for FakeTime {
            fn sleep(&self, delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
                self.1.lock().expect("sleeps").push(delay);
                *self.0.lock().expect("clock") += delay;
                Box::pin(async {})
            }
        }
        struct Contract;
        impl Transport for Contract {
            async fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, HttpError> {
                Ok(HttpResponse {
                    status: 200,
                    body: b"enum OperationType { OPERATION_TYPE_INPUT = 1; }".to_vec(),
                    retry_after: None,
                })
            }
        }

        let time = Arc::new(FakeTime(Mutex::new(Instant::now()), Mutex::new(Vec::new())));
        let gateway = Gateway::with_parts(
            Contract,
            BUDGETS,
            Arc::clone(&time) as Arc<dyn Clock>,
            Arc::clone(&time) as Arc<dyn Sleeper>,
        )
        .expect("the documented table is valid");

        for _ in 0..2 {
            gateway
                .send("operations.proto", &operation_types_request(), None)
                .await
                .expect("the contract host has a budget");
        }

        assert_eq!(*time.1.lock().expect("sleeps"), [Duration::from_secs(1)]);
    }
}
