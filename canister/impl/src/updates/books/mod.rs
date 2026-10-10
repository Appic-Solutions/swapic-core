//! Writes to the canister's USDC books.

use candid::Nat;
use ic_cdk::update;

/// Controller-only. Moves a surplus the last balance check found on `chain_id`, once
/// someone has looked at it, into the `unattributed` line.
#[update]
pub fn book_surplus(chain_id: u64, amount: Nat) -> Result<(), String> {
    let _ = (chain_id, amount);
    Err("not yet".to_string())
}
