use crate::chain::ChainId;
use crate::numeric::{EventIndex, Timestamp, TokenAmount};
use minicbor::{Decode, Encode};

/// What the fold keeps besides its maps: where the log stands and the pool's pause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct LedgerMeta {
    #[n(0)]
    pub fees_accrued: TokenAmount,
    /// The index the next event is written at.
    #[n(1)]
    pub next_event_index: EventIndex,
    /// The chain whose balance check found a shortfall, which paused the pool until a
    /// controller reopens it.
    #[n(3)]
    pub pool_paused_by: Option<ChainId>,
    /// When a controller paused the pool, while it stays paused.
    #[n(4)]
    pub pool_paused_at: Option<Timestamp>,
}

crate::storable_as_cbor!(LedgerMeta);
