use settlement_api::types::events::Hash32;
use settlement_api::types::swap::SwapState;
use std::collections::BTreeMap;

pub mod pending_quotes;
pub mod transitions;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Pocket {
    pub available: u128,
    pub reserved: u128,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct AppState {
    pub swaps: BTreeMap<Hash32, SwapState>,
    pub pockets: BTreeMap<u64, Pocket>,
    pub fees_accrued: u128,
    pub next_event_index: u64,
    pub last_event_hash: Hash32,
}
