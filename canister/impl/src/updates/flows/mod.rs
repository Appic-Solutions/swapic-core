//! The flow doors: the gasless relay, the CCTP finish, and their settings.

use crate::guards::require_controller;
use crate::relay;
use crate::storage::flow_settings;
use ic_cdk::update;
pub use settlement_api::types::chain_data::ChainData;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::flow::{FlowCallError, FlowSettings};
use types::ChainId;

/// Controller-only. The ck canisters, the relay window and the rebalancing's addresses.
#[update]
pub fn set_flow_settings(settings: FlowSettings) -> Result<(), FlowCallError> {
    require_controller().map_err(FlowCallError::Guard)?;
    flow_settings::set(settings).map_err(FlowCallError::Invalid)
}

/// Watcher-only (the submit API runs as the watcher), refused while halted. Relays a
/// user's gasless order through Swapic on `chain_id` as `runFor(order, auth)`: `order` is
/// `abi.encode(Order)` of ABI v3 and `auth` is `abi.encode(GaslessAuth)`, each one dynamic
/// tuple with its leading offset word, as viem's `encodeAbiParameters` writes it. The
/// order id is recomputed from the order and the chain's Swapic (the 3009 door's nonce
/// must be it); the order must pay this canister exactly one fee step of at least the
/// chain's `min_network_fee` for its rail, plus on CCTP the destination's for the arrival;
/// it is signed with exactly `gas_limit` (the limit the quoter charged for), refused when
/// that is above the chain's maximum for the action, and held to the gas rule. Signed
/// once per order id: a second call answers the first one's id. An order whose memo is
/// longer than 256 bytes is refused. Answers the signed transaction's id in `signed_txs`.
///
/// `reading`, when given, is `chain_id`'s head and gas price the watcher priced the relay
/// on, stored right after the role and the halt; an invalid one refuses the call. With none
/// the canister reads the gas price itself. A reading whose block is not above the stored
/// one's is kept out.
#[update]
pub async fn relay_gasless(
    chain_id: u64,
    order: Vec<u8>,
    auth: Vec<u8>,
    gas_limit: u64,
    reading: Option<ChainData>,
) -> Result<u64, String> {
    relay::relay_gasless(ChainId::new(chain_id), &order, &auth, gas_limit, reading).await
}

pub use settlement_api::types::flow::Recipe;

/// Watcher-only, refused while halted. Finishes a CCTP arrival on `chain_id`, the
/// destination. `message` and `attestation` are Circle's, the message 696 bytes with the
/// v3 envelope as its hook data; `src_tx_hash` is the source transaction that burned it,
/// which the watcher has proven paid the canister's fee (I-3.4); `gas_limit` is the limit
/// the quoter charged for. From the envelope and the clock: a Deliver is received on the
/// message transmitter; a Swap (with `recipe`, required) or a CkDeposit (with none) is
/// completed on the CctpReceiver before its deadline, and released after it. Signed with
/// exactly `gas_limit` and refused above the chain's maximum for the action, but a release,
/// which is signed at that maximum; held to the gas rule; once per arrival (the burn, by
/// its source domain and Circle's nonce, so a re-attested message is the same arrival)
/// and action, and one arrival per source transaction.
/// Answers the signed transaction's id in `signed_txs`.
///
/// `reading`, when given, is `chain_id`'s head and gas price the watcher priced the finish
/// on, stored right after the role and the halt; an invalid one refuses the call. With none
/// the canister reads the gas price itself. A reading whose block is not above the stored
/// one's is kept out.
#[update]
pub async fn finish_cctp(
    chain_id: u64,
    message: Vec<u8>,
    attestation: Vec<u8>,
    recipe: Option<Recipe>,
    src_tx_hash: Vec<u8>,
    gas_limit: u64,
    reading: Option<ChainData>,
) -> Result<u64, String> {
    use crate::cctp::{parse_address, FinishError, GivenRecipe};
    let recipe = recipe
        .map(|recipe| {
            Ok::<_, FinishError>(GivenRecipe {
                target: parse_address(&recipe.target)
                    .ok_or(FinishError::RecipeAddress("target"))?,
                data: recipe.data,
                scaler: recipe
                    .scaler
                    .map(|scaler| {
                        parse_address(&scaler).ok_or(FinishError::RecipeAddress("scaler"))
                    })
                    .transpose()?,
            })
        })
        .transpose()
        .map_err(|e| e.to_string())?;
    crate::cctp::finish_cctp(
        ChainId::new(chain_id),
        &message,
        &attestation,
        recipe,
        &src_tx_hash,
        gas_limit,
        reading,
    )
    .await
}
