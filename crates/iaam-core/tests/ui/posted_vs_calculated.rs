//! Posted amounts and calculated values are not mixed (§3.4).

use iaam_core::money::PostedMinor;
use iaam_core::numeric::decimal::Dec;

fn main() {
    let _: Dec = PostedMinor::new(100);
}
