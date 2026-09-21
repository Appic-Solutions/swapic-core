use crate::storage::chain_data;
use ic_cdk::query;
pub use settlement_api::types::chain_data::ChainDataEntry;
use types::ChainId;

/// The cached reading for a chain, with the instant the canister stamped it, or nothing for
/// a chain the watcher has never pushed. World-readable: a block height and a gas price are
/// public facts about a public chain, and the stamp is how an operator sees the watcher
/// stop pushing.
///
/// Answers the entry however old it is. Freshness is a decision, and the canister makes it
/// against `chain_data_max_age` at the moment it decides.
#[query]
pub fn get_chain_data(chain_id: u64) -> Option<ChainDataEntry> {
    chain_data::get(ChainId::new(chain_id)).map(ChainDataEntry::from)
}
