//! Рыночные данные: MOEX ISS и ЦБ РФ (§12).
//!
//! Крейта **описывает запрос и разбирает ответ**. HTTP она не знает —
//! транспорт живёт в `iaam-http`, и это охраняется правилом 11
//! `scripts/check-architecture.sh`. Отсюда главное свойство: разбор
//! проверяется на замороженных эталонах **без сети и без подмены HTTP**.
//!
//! Крейта не решает, какую цену применить: она отдаёт все наблюдения,
//! какие дал источник. Выбор между ними — политика оценки (E3.3).

pub mod cbr;
pub mod error;
pub mod moex;
pub mod observation;
pub mod schedule;

pub use error::MarketError;
pub use observation::{
    Executability, FxObservation, KeyRateObservation, ObservedAt, PriceKind, PriceObservation,
    TradeDate, Venue,
};
pub use schedule::{
    CouponAmount, CouponPeriod, Knowledge, OfferWindow, PrincipalRepayment, ScheduleSnapshot,
};
