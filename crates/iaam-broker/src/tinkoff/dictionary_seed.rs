//! Начальный словарь видов операций T-Invest (§14).
//!
//! Это **наше** знание, а не брокерское: контракт перечисляет коды,
//! но не сообщает, что `OPERATION_TYPE_DIV_EXT` и
//! `OPERATION_TYPE_DIVIDEND` для нас оба дивиденд, а
//! `OPERATION_TYPE_OVER_COM` — комиссия. Поэтому таблица живёт в коде
//! и попадает в базу один раз, при заведении доступа.
//!
//! Дальше она **не** источник истины: словарь редактируется в базе,
//! и пополнение отсюда существующие строки не трогает. Иначе решение
//! владельца отменялось бы при каждом заведении доступа.
//!
//! Содержимое воспроизводит прежний `match` из `parse.rs` (коммит
//! 5320fb0) слово в слово. Пропущенный синоним здесь — это код,
//! который после переезда молча стал бы неизвестным, и импорт перестал
//! бы разбирать то, что разбирал вчера.
//!
//! Амортизации и погашения здесь нет намеренно: их коды
//! (`OPERATION_TYPE_BOND_REPAYMENT`, `OPERATION_TYPE_BOND_REPAYMENT_FULL`)
//! контракт объявляет, но канал не сообщает ни возвращённого номинала
//! на единицу, ни места хранения — построить из них факт нечем.
//! Внести их владелец сможет решением, когда появится чем.

/// Пары «код канала → имя вида» для первого заселения.
pub const TINKOFF_OPERATION_KINDS: &[(&str, &str)] = &[
    ("OPERATION_TYPE_BUY", "buy"),
    ("OPERATION_TYPE_BUY_CARD", "buy"),
    ("OPERATION_TYPE_BUY_MARGIN", "buy"),
    ("OPERATION_TYPE_DELIVERY_BUY", "buy"),
    ("OPERATION_TYPE_SELL", "sell"),
    ("OPERATION_TYPE_SELL_CARD", "sell"),
    ("OPERATION_TYPE_SELL_MARGIN", "sell"),
    ("OPERATION_TYPE_DELIVERY_SELL", "sell"),
    ("OPERATION_TYPE_DIVIDEND", "dividend"),
    ("OPERATION_TYPE_DIV_EXT", "dividend"),
    ("OPERATION_TYPE_COUPON", "coupon"),
    ("OPERATION_TYPE_BROKER_FEE", "commission"),
    ("OPERATION_TYPE_SERVICE_FEE", "commission"),
    ("OPERATION_TYPE_MARGIN_FEE", "commission"),
    ("OPERATION_TYPE_SUCCESS_FEE", "commission"),
    ("OPERATION_TYPE_TRACK_MFEE", "commission"),
    ("OPERATION_TYPE_TRACK_PFEE", "commission"),
    ("OPERATION_TYPE_CASH_FEE", "commission"),
    ("OPERATION_TYPE_OUT_FEE", "commission"),
    ("OPERATION_TYPE_OUT_STAMP_DUTY", "commission"),
    ("OPERATION_TYPE_OUTPUT_PENALTY", "commission"),
    ("OPERATION_TYPE_ADVICE_FEE", "commission"),
    ("OPERATION_TYPE_OVER_COM", "commission"),
    ("OPERATION_TYPE_INPUT", "deposit"),
    ("OPERATION_TYPE_INPUT_SECURITIES", "deposit"),
    ("OPERATION_TYPE_INPUT_SWIFT", "deposit"),
    ("OPERATION_TYPE_INPUT_ACQUIRING", "deposit"),
    ("OPERATION_TYPE_INP_MULTI", "deposit"),
    ("OPERATION_TYPE_OUTPUT", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_SECURITIES", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_SWIFT", "withdrawal"),
    ("OPERATION_TYPE_OUTPUT_ACQUIRING", "withdrawal"),
    ("OPERATION_TYPE_OUT_MULTI", "withdrawal"),
    ("OPERATION_TYPE_TRANS_IIS_BS", "transfer"),
    ("OPERATION_TYPE_TRANS_BS_BS", "transfer"),
];

/// Как назвать источник этих строк в записи о происхождении.
pub const TINKOFF_SEED_NAME: &str = "встроенный словарь t-invest";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_kind::{ChannelOperationKind, OperationKindDictionary};

    /// Каждое имя вида обязано быть известно сборке: строка, которую
    /// не читает `parse`, легла бы в базу и стала бы отказом импорта,
    /// а не заселением.
    #[test]
    fn every_seeded_kind_is_readable_by_this_build() {
        let (_, unreadable) =
            OperationKindDictionary::build(TINKOFF_OPERATION_KINDS.iter().copied());
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    /// Код не может значить два разных вида. Дубль с расхождением
    /// молча выиграл бы порядком вставки.
    #[test]
    fn no_code_is_listed_twice() {
        let mut seen = std::collections::BTreeMap::new();
        for (code, kind) in TINKOFF_OPERATION_KINDS {
            if let Some(previous) = seen.insert(*code, *kind) {
                assert_eq!(previous, *kind, "код {code} назван дважды и по-разному");
                panic!("код {code} перечислен дважды");
            }
        }
    }

    /// Заслон против тихой потери синонима: словарь обязан разбирать
    /// ровно то, что разбирал прежний `match`. Проверяются те коды,
    /// на которых потеря заметна не сразу, — синонимы.
    #[test]
    fn the_synonyms_that_the_old_match_knew_are_all_here() {
        let (dictionary, _) =
            OperationKindDictionary::build(TINKOFF_OPERATION_KINDS.iter().copied());
        for (code, expected) in [
            ("OPERATION_TYPE_DIV_EXT", ChannelOperationKind::Dividend),
            ("OPERATION_TYPE_DELIVERY_BUY", ChannelOperationKind::Buy),
            ("OPERATION_TYPE_DELIVERY_SELL", ChannelOperationKind::Sell),
            ("OPERATION_TYPE_OVER_COM", ChannelOperationKind::Commission),
            ("OPERATION_TYPE_INP_MULTI", ChannelOperationKind::Deposit),
            ("OPERATION_TYPE_OUT_MULTI", ChannelOperationKind::Withdrawal),
            (
                "OPERATION_TYPE_TRANS_IIS_BS",
                ChannelOperationKind::Transfer,
            ),
        ] {
            assert_eq!(dictionary.kind_of(code), expected, "{code}");
        }
    }
}
