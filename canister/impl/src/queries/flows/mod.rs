//! The flows' settings and the signed transactions.

use crate::state::Store;
use crate::storage::events::{self, read_state};
use crate::storage::flow_settings;
use ic_cdk::query;
pub use settlement_api::types::flow::FlowSettings;
pub use settlement_api::types::tx::{ChainRef, SignedTx};
use types::EventIndex;

/// The flows' settings.
#[query]
pub fn get_flow_settings() -> FlowSettings {
    flow_settings::get_wire()
}

/// Every transaction the canister signed after `after_id`, oldest first, at most `limit`
/// of them (capped). The watcher broadcasts them; the canister's own broadcast is the
/// backup.
#[query]
pub fn signed_txs(after_id: u64, limit: u64) -> Vec<SignedTx> {
    let Some(from) = EventIndex::new(after_id).next() else {
        return Vec::new();
    };
    let limit = usize::try_from(limit.min(MAX_SIGNED_PAGE)).unwrap_or(0);
    read_state(|state| state.store().signed_in(from.., limit))
        .into_iter()
        .filter_map(|index| events::event_at(index.get()))
        .filter_map(|event| {
            let (chain_id, tx_hash, raw) = event.payload.signed_tx()?;
            Some(SignedTx {
                id: event.index.get(),
                chain: ChainRef::Evm(chain_id.get()),
                raw: raw.to_vec(),
                hash: tx_hash.into_bytes().to_vec(),
                signed_at_ns: event.timestamp.as_nanos(),
            })
        })
        .collect()
}

/// The most transactions one `signed_txs` page holds.
const MAX_SIGNED_PAGE: u64 = 100;

#[cfg(test)]
mod tests;
