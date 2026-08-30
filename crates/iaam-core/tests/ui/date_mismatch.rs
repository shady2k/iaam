//! Six semantic dates—six distinct types (§4.2).

use iaam_core::dates::{SettledDate, TradeDate};
use time::macros::date;

fn main() {
    let _: SettledDate = TradeDate(date!(2026 - 01 - 01));
}
