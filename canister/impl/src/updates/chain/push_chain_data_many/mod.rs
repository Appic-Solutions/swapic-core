//! The watcher's push for every chain at once: one message carrying each chain's reading,
//! each entry taken as `push_chain_data` takes one, so the canister pays one update's fee
//! for the whole push rather than one per chain.

#[cfg(test)]
mod tests;

use super::push_chain_data::store_reading;
use crate::guards;
use ic_cdk::update;
pub use settlement_api::types::chain_data::ChainPush;
use settlement_api::types::errors::{GuardError, PushChainDataError};
use types::{ChainId, Timestamp};

/// The most entries one call takes: every chain the canister names, several times over,
/// and still a small message.
pub const MAX_PUSHES: usize = 64;

// the interface promises at least 32 entries a call
const _: () = assert!(MAX_PUSHES >= 32);

/// Watcher-only. Every chain's reading in one message: each entry validated and stored
/// exactly as push_chain_data stores one, stamped with the canister's clock. One result per
/// entry, in order, each exactly push_chain_data's result; the role is checked once, and when
/// it fails every entry carries that Guard error. A chain named twice is stored twice, in
/// order, so the later reading is the one kept. A call with more than 64 entries is refused
/// whole: the canister traps and stores nothing. Not gated by the halt, as push_chain_data is
/// not.
#[update]
pub fn push_chain_data_many(pushes: Vec<ChainPush>) -> Vec<Result<(), PushChainDataError>> {
    if pushes.len() > MAX_PUSHES {
        ic_cdk::trap(&format!(
            "push_chain_data_many takes at most {MAX_PUSHES} entries, not {}",
            pushes.len()
        ));
    }
    store_all(
        guards::require_watcher(),
        pushes,
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
}

/// `pushes` stored in order, stamped `at`, each entry answering for itself, when `role`
/// passed; every entry refused with the role's error, and nothing stored, when it did not.
pub fn store_all(
    role: Result<(), GuardError>,
    pushes: Vec<ChainPush>,
    at: Timestamp,
) -> Vec<Result<(), PushChainDataError>> {
    match role {
        Err(error) => pushes
            .iter()
            .map(|_| Err(PushChainDataError::Guard(error.clone())))
            .collect(),
        Ok(()) => pushes
            .into_iter()
            .map(|push| store_reading(ChainId::new(push.chain_id), push.data, at))
            .collect(),
    }
}
