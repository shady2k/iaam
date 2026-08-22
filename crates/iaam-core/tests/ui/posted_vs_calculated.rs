//! Проведённые суммы и расчётные величины не смешиваются (§3.4).

use iaam_core::money::PostedMinor;
use iaam_core::numeric::decimal::Dec;

fn main() {
    let _: Dec = PostedMinor::new(100);
}