use crate::guards::require_watcher;
use crate::state::Store;
use crate::storage::{eco_intents, events};
use ic_cdk::update;
pub use settlement_api::types::entry::{EcoIntent, PushEcoIntentError};
pub use settlement_api::types::events::Hash32;
use types::rail::EcoIntentError;
use types::{ChainId, Quote, QuoteHash, Rail, UnixSeconds};

/// Watcher-only. Hands in what Eco's quote response gave a swap on the Eco rail: the
/// destination Eco named, the route, the reward's deadline and the prover. The inbox holds
/// one intent per swap, and a push replaces the one before it, so a corrected intent is the
/// one the publish carries. Rail data and not money: the vault locks only the swap's own
/// amount whatever the intent says, so the halt switch does not gate it. Only a known swap
/// on the Eco rail has an inbox slot.
#[update]
pub fn push_eco_intent(quote_hash: Hash32, intent: EcoIntent) -> Result<(), PushEcoIntentError> {
    require_watcher().map_err(PushEcoIntentError::Guard)?;
    let quote_hash = QuoteHash::new(quote_hash);
    let swap = events::read_state(|state| state.store().swap(&quote_hash))
        .ok_or(PushEcoIntentError::UnknownSwap(quote_hash.into_bytes()))?;
    let on_eco = Quote::parse(&swap.quote_bytes).is_ok_and(|quote| quote.rail == Rail::Eco);
    if !on_eco {
        return Err(PushEcoIntentError::NotAnEcoSwap(quote_hash.into_bytes()));
    }
    let prover = intent
        .prover
        .parse()
        .map_err(
            |reason: types::evm::EvmAddressError| PushEcoIntentError::ProverNotAnAddress {
                reason: reason.into(),
            },
        )?;
    let intent = types::EcoIntent::new(
        ChainId::new(intent.destination_chain),
        intent.route,
        UnixSeconds::new(intent.deadline_s),
        prover,
    )
    .map_err(
        |EcoIntentError::RouteTooLong { len, cap }| PushEcoIntentError::RouteTooLong {
            len: len as u64,
            cap: cap as u64,
        },
    )?;
    eco_intents::put(quote_hash, intent);
    Ok(())
}
