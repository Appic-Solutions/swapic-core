use std::collections::BTreeMap;
use types::{ChainId, EventHash, EventIndex, Pocket, QuoteHash, Swap, TokenAmount};

pub mod pending_quotes;
pub mod transitions;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppState {
    pub swaps: BTreeMap<QuoteHash, Swap>,
    pub pockets: BTreeMap<ChainId, Pocket>,
    pub fees_accrued: TokenAmount,
    pub next_event_index: EventIndex,
    pub last_event_hash: EventHash,
}
