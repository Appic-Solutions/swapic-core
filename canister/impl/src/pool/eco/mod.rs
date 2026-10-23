//! Eco intents the canister signs or relies on (P11e): every intent the canister funds (a
//! payout's onward intent, a move's proposed route) names Eco's configured prover and
//! Portal, and short deadlines; and the reads of Eco's Portal the canister makes itself
//! before it trusts a refund or rescues an unfilled intent: the destination's `claimants`
//! (read by every node, at a block past the route's deadline: M-03, L-03), the source's
//! `getRewardStatus`.

#[cfg(test)]
mod tests;

use crate::agreed::{self, FillRead};
use crate::evm_reads::{self, Simulated};
use types::eco::{self, Fill};
use types::swapic::EcoIntent;
use types::{ChainId, EvmAddress};

/// The latest an intent's route may be filled, from now.
pub const MAX_ROUTE_LIFE_S: u64 = 3_600;

/// The latest an unfilled intent's reward may come back: Eco's own week, and a day.
pub const MAX_REWARD_LIFE_S: u64 = 8 * 24 * 3_600;

/// The least an intent's reward may come back after its route can last be filled (L-03):
/// a refund in the seconds the two chains' clocks disagree could meet a fill.
pub const MIN_REFUND_GAP_S: u64 = 3_600;

/// The rules an intent the canister funds meets, pure: Eco's configured prover (any other
/// could pay the reward to whoever it says) and Portal (where its fill is proven), a route
/// that may be filled once the order runs and within the hour, a reward that comes back
/// an hour or more after the route and within Eco's week.
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
    if intent.reward_deadline < intent.route_deadline.saturating_add(MIN_REFUND_GAP_S)
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

/// When the Eco intent `pinned` publishes can last be filled: its route's deadline, or, when
/// the route does not parse, its order's deadline and the longest route the canister signs.
/// The one rule the payouts' queue and a move's refund read a fill past.
pub fn fill_read_after(pinned: &types::pool::PinnedOrder) -> u64 {
    crate::pool::rebalance::drive::eco_route_deadline(&pinned.order)
        .unwrap_or_else(|| pinned.deadline_s.saturating_add(MAX_ROUTE_LIFE_S))
}

/// What `chain`'s Portal says of `intent`'s fill, read by every node at a block whose time
/// is past the intent's `route_deadline` (no fill can come after it): Err while no agreed
/// block is, or when it was not read.
pub async fn fill(chain: ChainId, intent: [u8; 32], route_deadline: u64) -> Result<Fill, String> {
    match agreed::fill_after(chain, intent, route_deadline).await {
        Ok((fill, _)) => Ok(fill),
        Err(FillRead::TooEarly) => {
            Err("no block the nodes agree on is past the route's deadline yet".to_string())
        }
        // the kind only, as the read says it: an outcall's text can carry the url and key
        Err(FillRead::Unread(why)) => Err(format!("the fill read: {why}")),
    }
}

/// The one Eco intent `logs` show `portal` published, of `creator` when one is named: None
/// for none, and for two, which name no one intent. The one reader of a published intent
/// the rescue and the outbox share (CR-17); an `IntentPublished` another contract logs is
/// none.
pub fn published_intent(
    logs: &[crate::pool::arrival::Log],
    portal: EvmAddress,
    creator: Option<EvmAddress>,
) -> Option<types::swapic::PublishedIntent> {
    let mut intents = logs
        .iter()
        .filter(|log| {
            log.address == portal
                && log.topics.first() == Some(&types::swapic::topics::intent_published())
        })
        .filter_map(|log| types::swapic::read_intent_published(&log.topics, &log.data))
        .filter(|intent| creator.is_none_or(|creator| intent.creator == creator));
    let intent = intents.next()?;
    // two would name no one intent
    intents.next().is_none().then_some(intent)
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
