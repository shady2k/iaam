//! Identifiers for different entities are not interchangeable (§4.5).

use iaam_core::ids::{AccountId, OwnerId};

fn main() {
    let _: AccountId = OwnerId::new_random();
}
