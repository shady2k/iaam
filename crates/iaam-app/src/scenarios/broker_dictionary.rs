//! Сверка словаря видов операций с опубликованным контрактом (§14).
//!
//! Отвечает на один вопрос: какие коды брокер объявил, а словарь о них
//! не знает. Классифицировать их сверка не может и не пытается —
//! контракт перечисляет коды, но не сообщает, во что они превращаются
//! у нас. Смысл нового кода утверждает владелец.

use iaam_broker::tinkoff::contract::{operation_types_request, parse_operation_types};
use iaam_store::documents::BrokerCode;

use crate::AppServices;
use crate::error::AppError;
use crate::ports::{BrokerDictionary, OutboundHttp};

/// Итог сверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryGap {
    /// Сколько кодов объявляет контракт.
    pub declared: usize,
    /// Сколько из них словарь уже знает.
    pub known: usize,
    /// Коды, которых в словаре нет, поимённо.
    ///
    /// Именно поимённо, а не числом: «появилось три кода» не позволяет
    /// ни принять решение, ни даже понять, те ли это три, что были
    /// в прошлый раз.
    pub missing: Vec<String>,
}

/// Сверить словарь канала с контрактом.
///
/// Недоступность контракта — отказ, а не пустой список расхождений:
/// «сверили, всё на месте» и «сверить не удалось» обязаны различаться,
/// иначе первый же сбой сети выглядит как благополучие.
pub async fn compare_with_contract(
    http: &dyn OutboundHttp,
    dictionary: &dyn BrokerDictionary,
    broker: &BrokerCode,
) -> Result<DictionaryGap, AppError> {
    let response = http.send(operation_types_request()).await?;
    if !(200..=299).contains(&response.status) {
        return Err(AppError::Invalid {
            field: "contract".to_owned(),
            expected: "успешный ответ за контрактом".to_owned(),
            actual: response.status.to_string(),
        });
    }
    let body = String::from_utf8(response.body).map_err(|_| AppError::Invalid {
        field: "contract".to_owned(),
        expected: "текст контракта".to_owned(),
        actual: "не UTF-8".to_owned(),
    })?;
    let declared = parse_operation_types(&body).map_err(|error| AppError::Invalid {
        field: "contract".to_owned(),
        expected: "перечень OperationType".to_owned(),
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

/// Сверить словарь через собранные зависимости приложения.
///
/// Сервер зовёт этот фасад, а сценарий выше берёт порты по отдельности:
/// сверке нужны два из десяти, и требовать сборку целиком значило бы
/// требовать в тесте настроенный журнал ради чтения справочника.
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
                raw_hash: "контракт".to_owned(),
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
        BrokerCode::parse("tinkoff").expect("код брокера")
    }

    #[tokio::test]
    async fn the_codes_absent_from_the_dictionary_are_named_one_by_one() {
        let (http, known) = parts(CONTRACT, 200, &[("OPERATION_TYPE_INPUT", "deposit")]);
        let gap = compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
            .await
            .expect("сверка прошла");
        assert_eq!(gap.declared, 3);
        assert_eq!(gap.known, 1);
        assert_eq!(
            gap.missing,
            ["OPERATION_TYPE_BOND_REPAYMENT", "OPERATION_TYPE_COUPON"],
            "расхождение обязано называть коды, а не их число"
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
            .expect("сверка прошла");
        assert!(gap.missing.is_empty(), "{:?}", gap.missing);
        assert_eq!(gap.known, gap.declared);
    }

    /// «Сверили, всё на месте» и «сверить не удалось» обязаны
    /// различаться: иначе первый же сбой сети выглядит как благополучие,
    /// и словарь тихо отстаёт от контракта.
    #[tokio::test]
    async fn an_unavailable_contract_is_a_refusal_not_an_empty_gap() {
        let (http, known) = parts("", 503, &[]);
        assert!(
            compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
                .await
                .is_err()
        );
    }

    /// Ответ пришёл, но перечня в нём нет — тоже отказ: страница
    /// «репозиторий переехал» отдаётся кодом 200.
    #[tokio::test]
    async fn a_body_without_the_enum_is_a_refusal() {
        let (http, known) = parts("переехали, смотрите в другом месте", 200, &[]);
        assert!(
            compare_with_contract(http.as_ref(), known.as_ref(), &tinkoff())
                .await
                .is_err()
        );
    }
}
