//! Опубликованный контракт T-Invest: перечень видов операций (§14).
//!
//! Словарь видов живёт в данных, и сверять его есть с чем: T-Invest
//! публикует `operations.proto`, где перечень `OperationType` задан
//! исходным текстом. Сверка отвечает на один вопрос — какие коды
//! появились с прошлого раза.
//!
//! **Классифицировать новый код сверка не может и не пытается.**
//! Контракт называет коды, но не сообщает, во что они превращаются
//! у нас: `OPERATION_TYPE_OVERNIGHT` — это доход или комиссия, решает
//! владелец, а не текст протокола. Задача, подставившая бы догадку,
//! записала бы в словарь смысл, которого никто не утверждал.

use iaam_http::{Destination, HttpRequest};

/// Где лежит контракт.
///
/// Путь вынесен константой рядом с разбором: он часть того же ответа
/// на вопрос «откуда мы это взяли», что и сам перечень.
pub const OPERATIONS_CONTRACT_PATH: &str =
    "/RussianInvestments/investAPI/main/src/docs/contracts/operations.proto";

/// Как назвать источник словаря в записи о происхождении.
#[must_use]
pub fn contract_dictionary_name() -> String {
    format!("t-invest:{OPERATIONS_CONTRACT_PATH}")
}

/// Запрос за контрактом.
#[must_use]
pub fn operation_types_request() -> HttpRequest {
    HttpRequest::get(Destination::TinvestContract, OPERATIONS_CONTRACT_PATH)
}

/// Почему перечень не удалось прочитать.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("в контракте нет перечня OperationType")]
    EnumMissing,
    #[error("перечень OperationType не закрыт скобкой")]
    EnumUnterminated,
    /// Перечень найден, но пуст.
    ///
    /// Отдельная причина, а не пустой список: пустой список читается
    /// как «расхождений нет», то есть отказ разбора выглядел бы как
    /// успешная сверка.
    #[error("перечень OperationType пуст")]
    EnumEmpty,
}

/// Коды видов операций из текста контракта.
///
/// Разбор намеренно грубый: нас интересуют имена членов, а не
/// протокол целиком. Тянуть ради этого разборщик protobuf значило бы
/// внести зависимость, которая умеет несравнимо больше нужного.
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
        // Комментарий отбрасывается целиком: в нём попадаются те же
        // имена членов, и принятые за объявление они дали бы дубль.
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
  OPERATION_TYPE_INPUT = 1; // ввод средств
  OPERATION_TYPE_BOND_REPAYMENT_FULL = 6;
}
";

    #[test]
    fn the_members_are_read_in_the_order_the_contract_lists_them() {
        assert_eq!(
            parse_operation_types(SAMPLE).expect("перечень прочитан"),
            [
                "OPERATION_TYPE_UNSPECIFIED",
                "OPERATION_TYPE_INPUT",
                "OPERATION_TYPE_BOND_REPAYMENT_FULL",
            ]
        );
    }

    /// Имя члена в комментарии — не объявление. Принятое за объявление,
    /// оно добавило бы в словарь код, которого у брокера нет.
    #[test]
    fn a_name_inside_a_comment_is_not_a_member() {
        let codes = parse_operation_types(SAMPLE).expect("перечень прочитан");
        assert!(!codes.iter().any(|code| code.contains("FROM_A_COMMENT")));
    }

    #[test]
    fn a_contract_without_the_enum_is_a_refusal() {
        assert_eq!(
            parse_operation_types("enum SomethingElse { A = 1; }"),
            Err(ContractError::EnumMissing)
        );
    }

    /// Пустой перечень — отказ, а не пустой ответ: «расхождений нет»
    /// и «прочитать не удалось» обязаны различаться.
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
}
