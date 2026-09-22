use crate::guards::require_watcher;
use crate::state::Store;
use crate::storage::{attestations, events};
use ic_cdk::update;
pub use settlement_api::types::entry::PushAttestationError;
pub use settlement_api::types::events::Hash32;
use types::entry::AttestationError;
use types::{Attestation, QuoteHash, Timestamp};

/// Watcher-only. Hands in what Circle attested for a swap's burn: the message the source
/// chain emitted and the signatures over it, which the mint on the destination takes. The
/// inbox is stable memory and holds one attestation per swap: the same push again changes
/// nothing, and a different one replaces it, so a corrected attestation is the one the
/// mint carries. Rail data and not money, so the halt switch does not gate it, and a wrong
/// pair costs a reverted mint and nothing else. Only a known swap has an inbox slot.
#[update]
pub fn push_attestation(
    quote_hash: Hash32,
    message: Vec<u8>,
    attestation: Vec<u8>,
) -> Result<(), PushAttestationError> {
    require_watcher().map_err(PushAttestationError::Guard)?;
    let quote_hash = QuoteHash::new(quote_hash);
    if events::read_state(|state| state.store().swap(&quote_hash).is_none()) {
        return Err(PushAttestationError::UnknownSwap(quote_hash.into_bytes()));
    }
    let received_at = Timestamp::from_nanos(ic_cdk::api::time());
    let attestation = Attestation::new(message, attestation, received_at).map_err(|error| {
        let wire_len = |len: usize| len as u64;
        match error {
            AttestationError::MessageTooLong { len, cap } => PushAttestationError::MessageTooLong {
                len: wire_len(len),
                cap: wire_len(cap),
            },
            AttestationError::AttestationTooLong { len, cap } => {
                PushAttestationError::AttestationTooLong {
                    len: wire_len(len),
                    cap: wire_len(cap),
                }
            }
        }
    })?;
    attestations::put(quote_hash, attestation);
    Ok(())
}
