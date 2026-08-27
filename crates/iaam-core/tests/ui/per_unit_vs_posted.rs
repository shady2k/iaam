//! Величина на единицу и проведённая сумма не смешиваются (§3.4).
//!
//! Номинал на бумагу — договорная расчётная величина; сложить её
//! с суммой, проведённой по счёту, значит выдать одно за другое.

use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor};
use iaam_core::numeric::decimal::Dec;

fn main() {
    let nominal = PerUnitAmount::new(Dec::one(), CurrencyCode::Rub);
    let posted = Money::new(PostedMinor::new(100), CurrencyCode::Rub);
    let _ = posted.try_add(nominal);
}
