//! Eco intents the canister signs or relies on (P11e): every intent the canister funds (a
//! payout's onward intent, a move's proposed route) names Eco's configured prover and
//! Portal, and short deadlines; and the reads of Eco's Portal the canister makes itself
//! before it trusts a refund or rescues an unfilled intent: the destination's `claimants`, the
//! source's `getRewardStatus`.

#[cfg(test)]
mod tests;

use crate::evm_reads::{self, Simulated};
use types::eco::{self, Fill};
use types::swapic::EcoIntent;
use types::{ChainId, EvmAddress};

/// The latest an intent's route may be filled, from now.
pub const MAX_ROUTE_LIFE_S: u64 = 3_600;

/// The latest an unfilled intent's reward may come back: Eco's own week, and a day.
pub const MAX_REWARD_LIFE_S: u64 = 8 * 24 * 3_600;

/// The rules an intent the canister funds meets, pure: Eco's configured prover (any other
/// could pay the reward to whoever it says) and Portal (where its fill is proven), a route
/// that may be filled once the order runs and within the hour, a reward that comes back
/// after the route and within Eco's week.
pub fn check_intent(
    intent: &EcoIntent,
    portal: Option<EvmAddress>,
    prover: Option<EvmAddress>,
    order_deadline: u64,
    now_s: u64,
) -> Result<(), String> {
    match prover {
        Some(prover) if prover == intent.prover => {}
        Some(_) => return Err("the intent names another prover than Eco's".to_string()),
        None => return Err("no Eco prover is configured".to_string()),
    }
    if portal != Some(intent.portal) {
        return Err("the intent is filled on another Portal than Eco's".to_string());
    }
    if intent.route_deadline < order_deadline
        || intent.route_deadline > now_s.saturating_add(MAX_ROUTE_LIFE_S)
    {
        return Err("the intent's route deadline is not short".to_string());
    }
    if intent.reward_deadline < intent.route_deadline
        || intent.reward_deadline > now_s.saturating_add(MAX_REWARD_LIFE_S)
    {
        return Err("the intent's refund comes too late".to_string());
    }
    Ok(())
}

fn word_of(answer: Simulated) -> Result<[u8; 32], String> {
    match answer {
        Simulated::Returned(bytes) if bytes.len() == 32 => {
            Ok(bytes.try_into().expect("BUG: checked to be 32 bytes"))
        }
        Simulated::Returned(_) => Err("the Portal answered no word".to_string()),
        Simulated::Reverted(_) => Err("the Portal's read reverted".to_string()),
    }
}

/// What `chain`'s Portal says of `intent`'s fill, read at its head.
pub async fn fill(chain: ChainId, intent: [u8; 32]) -> Result<Fill, String> {
    let config = crate::storage::config::get();
    let portal = config.eco_portal.ok_or("no Eco Portal is configured")?;
    let canister = crate::storage::ecdsa_address::get().ok_or("no canister address yet")?;
    let answer = evm_reads::eth_call(chain, canister, portal, &eco::claimants_calldata(intent))
        .await
        // the kind only: an outcall's text can carry the provider's url and key
        .map_err(|error| format!("the fill read: {:?}", error.kind()))?;
    Ok(eco::fill_of(word_of(answer)?))
}

/// What `chain`'s Portal says of `intent`'s reward: 3 once refunded to its creator.
pub async fn reward_status(chain: ChainId, intent: [u8; 32]) -> Result<u8, String> {
    let config = crate::storage::config::get();
    let portal = config.eco_portal.ok_or("no Eco Portal is configured")?;
    let canister = crate::storage::ecdsa_address::get().ok_or("no canister address yet")?;
    let answer = evm_reads::eth_call(
        chain,
        canister,
        portal,
        &eco::reward_status_calldata(intent),
    )
    .await
    .map_err(|error| format!("the reward read: {:?}", error.kind()))?;
    Ok(word_of(answer)?[31])
}
