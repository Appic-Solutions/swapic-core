use crate::types::events::EventEnvelope;
use ic_stable_structures::storable::Bound;
use ic_stable_structures::Storable;
use std::borrow::Cow;

// Storage codec only. The hash chain uses the hand-written codec in `events`, so a
// candid layout change can never move a hash.
impl Storable for EventEnvelope {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(candid::encode_one(self).expect("envelope encodes"))
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        candid::decode_one(&bytes).expect("envelope decodes")
    }

    const BOUND: Bound = Bound::Unbounded;
}
