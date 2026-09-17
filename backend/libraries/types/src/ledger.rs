use crate::hash::EventHash;
use crate::numeric::{EventIndex, TokenAmount};
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
}

crate::storable_as_cbor!(LedgerMeta);
