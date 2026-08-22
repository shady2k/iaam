//! Три числовых режима (§6.6 спецификации).
//!
//! | Режим | Где | Тип |
//! |---|---|---|
//! | Точный | тождество результата, разнесение basis, сверка | [`exact::Exact`] |
//! | Денежный | суммы, цены, курсы, НКД | [`decimal::Dec`] |
//! | Приближённый | XIRR, CAGR, DCF — степени, корни, итерации | [`approx`] |
//!
//! Приближённые величины **никогда** не входят в денежное тождество:
//! тождество проверяет суммы, а не ставки.

pub mod approx;
pub mod decimal;
pub mod exact;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NumericError {
    #[error("знаменатель равен нулю")]
    ZeroDenominator,
    #[error("деление на ноль")]
    DivisionByZero,
    #[error("переполнение при точном вычислении")]
    Overflow,
    #[error("масштаб {scale} превышает поддерживаемый максимум {max}")]
    ScaleTooLarge { scale: u32, max: u32 },
}
