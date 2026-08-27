//! Доменные типы графика выплат (§2.1 спеки E3.4).
//!
//! Разрез идёт по роли строки в расчёте, а не по колонкам источника.
//! `CouponPeriod` даёт поток и базу не двигает; `PrincipalRepayment` даёт
//! поток и уменьшает непогашенный номинал; `OfferWindow` потока не даёт
//! вовсе — он даёт опцию. Общая таблица с видом строки заставила бы
//! каждого потребителя ветвиться по виду, то есть вернула бы тот `match`,
//! который вынесен из разборщика в словарь базы (миграция 0009).
//!
//! Ни один тип здесь не толкует коды источника: вид возврата номинала и
//! вид права по оферте хранятся так, как их назвал источник, и переводятся
//! словарём на границе приложения (§2.5).

use iaam_core::ids::InstrumentId;
use iaam_core::money::CurrencyCode;
use iaam_core::numeric::decimal::Dec;
use serde::{Deserialize, Serialize};
use time::Date;

use crate::observation::ObservedAt;

/// Знание об атрибуте: известен или неизвестен.
///
/// Отдельный тип, а не `Option`, намеренно: `Option` соблазняет на
/// `unwrap_or_default`, а подставленная по умолчанию база начисления дней
/// даёт правдоподобно неверный НКД, которого не покажет ни один тест на
/// бумаге с целым числом периодов.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge<T> {
    Known(T),
    Unknown,
}

impl<T> Knowledge<T> {
    /// Известное значение, если оно есть.
    ///
    /// Существует ради чтения; значения по умолчанию тут нет и не будет.
    pub const fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

/// Что известно о выплате за купонный период (§2.3).
///
/// Ноль — присутствующее числовое значение, отсутствие — его отрицание.
/// Подмена одного другим занижает и полученный поток, и YTM, и делает это
/// правдоподобно. Статус **не выводится из даты**: у проверенного флоатера
/// купон 2020 года пришёл без суммы и без ставки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouponAmount {
    /// Сумма на единицу первоначального номинала и её валюта известны.
    AmountFixed {
        per_unit: Dec,
        currency: CurrencyCode,
    },
    /// Ставка известна, сумма ещё нет.
    RateFixedAmountUndetermined { rate_percent: Dec },
    /// Ни того, ни другого.
    Undetermined,
}

/// Начисление дохода за купонный период.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouponPeriod {
    /// Начало периода. Эмитент его не двигает — в отличие от даты платежа.
    pub period_start: Date,
    /// Конец начисления. НКД считается по нему.
    pub accrual_end: Date,
    /// Дата платежа. Двигается переносом с выходного и правкой эмитента.
    pub payment_date: Date,
    /// Дата фиксации права. Источник её сообщает не всегда.
    pub record_date: Knowledge<Date>,
    pub amount: CouponAmount,
    /// Собственный идентификатор записи у источника.
    ///
    /// `Option`, потому что у MOEX его нет вовсе (§2.11). Отсутствие —
    /// нормальное состояние, а не пустое обязательное поле.
    pub source_entry_id: Option<String>,
}

/// Возврат части номинала на дату.
///
/// Окончательность возврата здесь **не хранится**: она выводится из
/// накопленной суммы долей (§2.1). Кода окончательности у источника может
/// не быть вовсе, а вывод, записанный наблюдением, запрещён ADR-0002.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRepayment {
    pub repayment_date: Date,
    /// Доля **первоначального** номинала, в процентах.
    pub share_percent: Dec,
    /// Как вид назвал источник. Здесь не толкуется.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Право предъявления к выкупу в окне.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferWindow {
    pub execution_date: Date,
    pub submission_start: Knowledge<Date>,
    pub submission_end: Knowledge<Date>,
    /// Цена выкупа в процентах номинала.
    pub price_percent: Knowledge<Dec>,
    pub agent: Knowledge<String>,
    /// Как вид права назвал источник. У MOEX это свободный русский текст.
    pub source_kind: String,
    pub source_entry_id: Option<String>,
}

/// Снимок графика выпуска целиком — единица наблюдения (§2.2).
///
/// Единицей служит снимок, а не строка, потому что построчная модель не
/// умеет выразить **исчезновение** строки: отсутствие новой версии по
/// старой координате неотличимо от «источник не присылал обновлений», и
/// отменённая амортизация остаётся рядом с новым графиком. Стабильного
/// идентификатора, которым эту беду обычно чинят, источник не даёт.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSnapshot {
    pub instrument: InstrumentId,
    pub observed_at: ObservedAt,
    pub coupon_periods: Vec<CouponPeriod>,
    pub principal_repayments: Vec<PrincipalRepayment>,
    pub offer_windows: Vec<OfferWindow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use time::macros::date;

    #[test]
    fn a_coupon_period_keeps_accrual_end_and_payment_date_apart() {
        // Перенос выплаты с выходного двигает дату платежа, но не конец
        // начисления. Одно поле на оба смысла теряет перенос молча, а НКД
        // считается по концу начисления.
        let period = CouponPeriod {
            period_start: date!(2026 - 02 - 15),
            accrual_end: date!(2026 - 08 - 15),
            payment_date: date!(2026 - 08 - 17),
            record_date: Knowledge::Unknown,
            amount: CouponAmount::Undetermined,
            source_entry_id: None,
        };
        assert_ne!(period.accrual_end, period.payment_date);
    }

    #[test]
    fn a_repayment_carries_a_share_not_an_amount() {
        // Сумма зависит от остатка номинала, а остаток выводится из
        // первоначального и ряда возвратов. Хранить сумму значило бы
        // завести второй источник истины рядом с выводом.
        let repayment = PrincipalRepayment {
            repayment_date: date!(2034 - 08 - 09),
            share_percent: Dec::new(Decimal::from(25)),
            source_kind: "amortization".to_owned(),
            source_entry_id: None,
        };
        assert_eq!(repayment.share_percent, Dec::new(Decimal::from(25)));
    }

    #[test]
    fn an_offer_window_without_dates_is_unknown_not_absent() {
        // Источник массово отдаёт окна без дат подачи и без цены.
        // Пустое окно — незнание условий, а не заявление, что окна нет.
        let window = OfferWindow {
            execution_date: date!(2027 - 08 - 26),
            submission_start: Knowledge::Unknown,
            submission_end: Knowledge::Unknown,
            price_percent: Knowledge::Unknown,
            agent: Knowledge::Unknown,
            source_kind: "Оферта".to_owned(),
            source_entry_id: None,
        };
        assert!(matches!(window.price_percent, Knowledge::Unknown));
    }
}
