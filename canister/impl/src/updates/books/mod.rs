//! Writes to the canister's USDC books.

use crate::guards::require_controller;
use candid::Nat;
use ic_cdk::update;
use types::books::BookStep;
use types::{ChainId, TokenAmount};

/// Controller-only. Moves a surplus the last balance check found on `chain_id`, once
/// someone has looked at it, into the `unattributed` line.
#[update]
pub fn book_surplus(chain_id: u64, amount: Nat) -> Result<(), String> {
    require_controller().map_err(|e| format!("{e:?}"))?;
    let amount =
        TokenAmount::try_from(amount).map_err(|_| "the amount is too large".to_string())?;
    crate::books::record(ChainId::new(chain_id), BookStep::SurplusBooked { amount })
        .map_err(|e| e.to_string())
}
