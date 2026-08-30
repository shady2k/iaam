//! A per-unit value and a posted amount are not mixed (§3.4).
//!
//! Face value per security is a contractual calculated value; adding it
//! to an amount posted to an account means conflating the two.

use iaam_core::money::{CurrencyCode, Money, PerUnitAmount, PostedMinor};
use iaam_core::numeric::decimal::Dec;

fn main() {
    let nominal = PerUnitAmount::new(Dec::one(), CurrencyCode::Rub);
    let posted = Money::new(PostedMinor::new(100), CurrencyCode::Rub);
    let _ = posted.try_add(nominal);
}
