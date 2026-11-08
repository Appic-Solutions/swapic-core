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
    // the watcher and the controllers read a signed transaction at once; anyone else only
    // once it is long sent (audit C5: a swap readable before it is sent can be sandwiched)
    let sees_all = crate::guards::require_watcher_or_controller().is_ok();
    signed_page(after_id, limit, sees_all, ic_cdk::api::time())
}

/// [`signed_txs`]' page for a reader who sees all (`sees_all`) or not, at `now` (nanoseconds).
pub fn signed_page(after_id: u64, limit: u64, sees_all: bool, now: u64) -> Vec<SignedTx> {
    let Some(from) = EventIndex::new(after_id).next() else {
        return Vec::new();
    };
    let limit = usize::try_from(limit.min(MAX_SIGNED_PAGE)).unwrap_or(0);
    read_state(|state| state.store().signed_in(from.., limit))
        .into_iter()
        .filter_map(|index| events::event_at(index.get()))
        .filter(|event| {
            sees_all
                || now.saturating_sub(event.timestamp.as_nanos())
                    >= crate::queries::events::HIDE_TX_BYTES_NS
        })
        .filter_map(|event| {
            // a Solana transaction's hash is its 64-byte signature
            if let types::EventType::Solana { step } = &event.payload {
                let (signature, raw) = step.signed()?;
                return Some(SignedTx {
                    id: event.index.get(),
                    chain: ChainRef::Solana,
                    raw: raw.to_vec(),
                    hash: signature.to_vec(),
                    signed_at_ns: event.timestamp.as_nanos(),
                });
            }
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
