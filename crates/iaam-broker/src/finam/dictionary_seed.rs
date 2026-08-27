//! Начальный словарь видов операций Finam (§14).
//!
//! Воспроизводит прежний `match` из `parse.rs` (коммит 5320fb0).
//! Коды здесь в верхнем регистре, потому что разбор канала приводит
//! их к нему: приведение — свойство этого канала, а не словаря.
//!
//! Канал приложением пока не подключён, и словарь ему не нужен
//! сегодня. Таблица заведена всё равно: знание о том, что `INTEREST`
//! у Finam — купон, а `TRADE_BUY` — покупка, иначе осталось бы только
//! в истории git, и подключающий канал восстанавливал бы его заново.

/// Пары «код канала → имя вида» для первого заселения.
pub const FINAM_OPERATION_KINDS: &[(&str, &str)] = &[
    ("BUY", "buy"),
    ("PURCHASE", "buy"),
    ("TRADE_BUY", "buy"),
    ("SELL", "sell"),
    ("TRADE_SELL", "sell"),
    ("DEPOSIT", "deposit"),
    ("CASH_DEPOSIT", "deposit"),
    ("INPUT", "deposit"),
    ("WITHDRAWAL", "withdrawal"),
    ("CASH_WITHDRAWAL", "withdrawal"),
    ("OUTPUT", "withdrawal"),
    ("DIVIDEND", "dividend"),
    ("COUPON", "coupon"),
    ("INTEREST", "coupon"),
    ("COMMISSION", "commission"),
    ("FEE", "commission"),
];

/// Как назвать источник этих строк в записи о происхождении.
pub const FINAM_SEED_NAME: &str = "встроенный словарь finam";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_kind::OperationKindDictionary;

    #[test]
    fn every_seeded_kind_is_readable_by_this_build() {
        let (_, unreadable) = OperationKindDictionary::build(FINAM_OPERATION_KINDS.iter().copied());
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    #[test]
    fn the_codes_are_upper_case_as_the_channel_leaves_them() {
        for (code, _) in FINAM_OPERATION_KINDS {
            assert_eq!(
                *code,
                code.to_ascii_uppercase(),
                "разбор канала отдаёт код в верхнем регистре, словарь обязан совпасть"
            );
        }
    }
}
