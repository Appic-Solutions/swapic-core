use crate::chain::ChainId;
use crate::hash::EventHash;
use crate::numeric::{EventIndex, Timestamp, TokenAmount};
use minicbor::{Decode, Encode};

/// What the fold keeps besides swaps and pockets.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct LedgerMeta {
    #[n(0)]
    pub fees_accrued: TokenAmount,
    /// The index the next event is sealed at.
    #[n(1)]
    pub next_event_index: EventIndex,
    /// The chain head the next event links to.
    #[n(2)]
    pub last_event_hash: EventHash,
    /// The chain whose balance check found a shortfall, which paused the pool until a
    /// controller reopens it.
    #[n(3)]
    pub pool_paused_by: Option<ChainId>,
    /// When a controller paused the pool, while it stays paused.
    #[n(4)]
    pub pool_paused_at: Option<Timestamp>,
}

crate::storable_as_cbor!(LedgerMeta);
