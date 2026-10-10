//! The flow doors: the gasless relay, the two ICP legs, and their settings.

use crate::guards::require_controller;
use crate::relay;
use crate::storage::flow_settings;
use ic_cdk::update;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::flow::{FlowCallError, FlowSettings};
use types::ChainId;

/// Controller-only. The routers, the ck canisters, the ICPSwap pools a quote may name,
/// and the windows the flows read.
#[update]
pub fn set_flow_settings(settings: FlowSettings) -> Result<(), FlowCallError> {
    require_controller().map_err(FlowCallError::Guard)?;
    flow_settings::set(settings).map_err(FlowCallError::Invalid)
}

/// Quoter-only, refused while halted. Relays a user's signed `swapFor` through the router
/// on `chain_id`: `params` and `auth` are the ABI encodings of the `SwapParams` and
/// `GaslessAuth` tuples. The quote hash is recomputed from the params as the router does,
/// every readable field is checked against them, the authorization must recover to the
/// payer, and `swapFor` must succeed in simulation before the canister signs it once and
/// records `GaslessRelayed`. Answers the quote hash; `relay_status` follows it.
#[update]
pub async fn relay_gasless(
    chain_id: u64,
    params: Vec<u8>,
    auth: Vec<u8>,
) -> Result<Hash32, FlowCallError> {
    relay::relay_gasless(ChainId::new(chain_id), &params, &auth)
        .await
        .map(|quote_hash| quote_hash.into_bytes())
}
