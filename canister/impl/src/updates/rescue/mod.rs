//! The Eco rescue's door.

use ic_cdk::update;
pub use settlement_api::types::pool::EcoRescueOk;

/// Watcher-only, refused while halted: for the Eco intent the order `order_id` published (a
/// relayed order's, a pool payout's or a move's: one the canister signed for), once its fill
/// deadline passed with no fill, the canister reads the order's own transaction and the
/// intent's destination Portal itself and signs `cancelAndProve` there, paying the proof's
/// fee (the `eco_rescue` gas row bounds both); Eco refunds the reward's creator on the source
/// by itself. One rescue per order: a second call answers the first one's id. Every rescue
/// is logged.
#[update]
pub async fn rescue_eco_intent(order_id: Vec<u8>) -> Result<EcoRescueOk, String> {
    crate::rescue::rescue(order_id)
        .await
        .map(|tx_id| EcoRescueOk { tx_id })
}
