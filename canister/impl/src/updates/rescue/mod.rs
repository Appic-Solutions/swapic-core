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

/// Controller-only, refused while halted: claims what Across's SpokePool on the pool chain
/// (`chain_id`) holds back for the canister (BSC design 6.2.9), once its
/// `getRelayerRefund(USDC, canister)` reads above zero, signed with the chain's
/// `across_claim` gas row. One claim at a time on the chain. The USDC it brings is credited
/// as any Across refund to the canister, to the payouts Across owes back first. Answers the
/// signed transaction's id in `signed_txs`.
#[update]
pub async fn claim_across_refund(chain_id: u64) -> Result<u64, String> {
    crate::pool::across::claim::claim(chain_id).await
}
