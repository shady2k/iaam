//! Раздельные идентичности (§4.5).
//!
//! Брокерский счёт не является одновременно владельцем, денежным счётом
//! и местом хранения бумаг: перевод бумаг между депозитариями внутри
//! одного брокера — реальная операция.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new_random() -> Self {
                Self(Uuid::new_v4())
            }

            #[must_use]
            pub const fn inner(&self) -> Uuid {
                self.0
            }
        }
    };
}

typed_id!(
    /// Владелец портфеля.
    OwnerId
);
typed_id!(
    /// Денежный счёт: брокерский, банковский, вклад, кошелёк.
    AccountId
);
typed_id!(
    /// Место хранения бумаг (депозитарий, субсчёт).
    CustodyId
);
typed_id!(
    /// Инструмент.
    InstrumentId
);
typed_id!(
    /// Источник данных: конкретный отчёт, синхронизация, ручной ввод.
    SourceId
);
typed_id!(
    /// Событие журнала.
    EventId
);
typed_id!(
    /// Перевод денег между счетами. Связывает обе стороны движения:
    /// без него классификатор контура не знает второй счёт (§4.10).
    TransferId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_the_uuid_they_wrap() {
        let raw = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(OwnerId(raw).inner(), raw);
        assert_eq!(AccountId(raw).inner(), raw);
        assert_eq!(CustodyId(raw).inner(), raw);
        assert_eq!(InstrumentId(raw).inner(), raw);
        assert_eq!(SourceId(raw).inner(), raw);
        assert_eq!(EventId(raw).inner(), raw);
        assert_eq!(TransferId(raw).inner(), raw);
    }

    #[test]
    fn ids_of_different_kinds_are_distinct_types() {
        // Несовместимость типов проверена исполнением: строка ниже даёт
        // E0308 «expected `AccountId`, found `OwnerId`». Постоянной
        // проверки на это НЕТ — она требует trybuild, которого в этом
        // плане не появляется; закомментированная строка её не заменяет.
        // let _: AccountId = OwnerId::new_random();
        let a = AccountId::new_random();
        let b = AccountId::new_random();
        assert_ne!(a, b, "два случайных идентификатора не совпадают");
    }
}
