//! Деньги нельзя сложить в обход проверки валюты (§15.1).

use iaam_core::money::{CurrencyCode, Money, PostedMinor};

fn main() {
    let rubles = Money::new(PostedMinor::new(100), CurrencyCode::Rub);
    let dollars = Money::new(PostedMinor::new(100), CurrencyCode::Usd);
    let _ = rubles + dollars;
}