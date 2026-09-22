use crate::guards::require_watcher;
use crate::rails::cctp::Cctp;
use crate::rails::Position;
use crate::state::Store;
use crate::storage::{attestations, config, ecdsa_address, events};
use ic_cdk::update;
pub use settlement_api::types::entry::PushAttestationError;
pub use settlement_api::types::events::Hash32;
use types::cctp::BurnMessage;
use types::entry::AttestationError;
use types::{Attestation, Leg as SwapLeg, Outcome, Quote, QuoteHash, Timestamp, TxHash};

/// Watcher-only. Hands in what Circle attested for a swap's burn: the message the source
/// chain emitted and the signatures over it, which the mint on the destination takes,
/// named by the burn transaction the watcher fetched it for.
///
/// The message is bound to the swap before it is taken in: the swap must be on a CCTP
/// rail with its burn confirmed, the burn named must be the transaction that burn
/// confirmed as, and every field of the message that the burn determined must be the
/// swap's own (the lane, the token, the amount, the destination vault, this canister as
/// the caller, the rail's threshold and fee ceiling). A message of another burn, ours or
/// anyone's, is refused by the field, so it can never be minted under this swap's name
/// and paid out of the destination vault's pooled balance. The inbox is stable memory
/// and holds one attestation per swap: the same push again changes nothing, and a
/// different one that binds replaces it. Rail data and not money, so the halt switch
/// does not gate it.
#[update]
pub fn push_attestation(
    quote_hash: Hash32,
    burn_tx_hash: Hash32,
    message: Vec<u8>,
    attestation: Vec<u8>,
) -> Result<(), PushAttestationError> {
    require_watcher().map_err(PushAttestationError::Guard)?;
    let quote_hash = QuoteHash::new(quote_hash);
    let swap = events::read_state(|state| state.store().swap(&quote_hash))
        .ok_or(PushAttestationError::UnknownSwap(quote_hash.into_bytes()))?;
    let quote = Quote::parse(&swap.quote_bytes)
        .map_err(|_| PushAttestationError::NotACctpSwap(quote_hash.into_bytes()))?;
    let rail =
        Cctp::of(quote.rail).ok_or(PushAttestationError::NotACctpSwap(quote_hash.into_bytes()))?;
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
    let burned =
        swap.last_leg == Some(SwapLeg::Burn) && swap.last_outcome == Some(Outcome::Confirmed);
    let confirmed = match swap.last_tx_hash {
        Some(confirmed) if burned => confirmed,
        _ => {
            return Err(PushAttestationError::NoBurnConfirmed(
                quote_hash.into_bytes(),
            ))
        }
    };
    if confirmed != TxHash::new(burn_tx_hash) {
        return Err(PushAttestationError::NotTheSwapsBurn {
            pushed: burn_tx_hash,
            confirmed: confirmed.into_bytes(),
        });
    }
    let config = config::get();
    let mine = ecdsa_address::get().ok_or(PushAttestationError::AddressNotDerived)?;
    let at = Position {
        quote_hash,
        quote: &quote,
        swap: &swap,
        config: &config,
        mine,
        attestation: None,
        intent: None,
        now: received_at.as_secs(),
    };
    let parsed = BurnMessage::parse(attestation.message())
        .map_err(|error| PushAttestationError::Rail(crate::rails::RailError::from(error).into()))?;
    rail.ensure_message_binds(&at, &parsed)
        .map_err(|error| PushAttestationError::Rail(error.into()))?;
    attestations::put(quote_hash, attestation);
    Ok(())
}
