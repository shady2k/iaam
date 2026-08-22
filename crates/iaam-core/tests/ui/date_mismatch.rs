//! Шесть семантических дат — шесть разных типов (§4.2).

use iaam_core::dates::{SettledDate, TradeDate};
use time::macros::date;

fn main() {
    let _: SettledDate = TradeDate(date!(2026 - 01 - 01));
}