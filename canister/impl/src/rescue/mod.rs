//! The Eco rescue (Q8 of the 2026-10-17 decisions, P11e): once an Eco intent's fill deadline
//! passed with no fill, the watcher asks, and the canister signs Eco's `cancelAndProve` on the
//! intent's destination, paying the proof's fee itself; Eco then refunds the reward's creator
//! (the user, or the canister for its own payouts and moves) on the source by itself.
//!
//! `rescue_eco_intent(order_id)`, watcher only. The canister trusts nothing it is told: it
//! finds the order's own source transaction in its records (a relayed order's, a pool
//! payout's, a move's), reads the `IntentPublished` there itself, checks the intent's hash,
//! Eco's prover and that its route's deadline passed, reads the destination Portal itself (no
//! claimant: never filled nor cancelled) and the source's reward (not refunded already), and
//! asks the prover for the proof's fee. One rescue per order: a second call answers the first
//! one's transaction. Every rescue is logged, as it is signed and as it closes.

#[cfg(test)]
mod tests;

use crate::guards::{require_not_halted, require_watcher};
use crate::pool::arrival::{read_source, Receipt};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::{config, flow_settings};
use crate::tx;
use types::config::GasAction;
use types::eco::{self, Fill, Reward};
use types::events::TxPurpose;
use types::hash::{PoolRef, QuoteHash};
use types::pool::MoveDirection;
use types::swapic::{self, PublishedIntent};
use types::{ChainId, EvmAddress, Timestamp, TxHash, Wei};

/// How long past the route's deadline a rescue waits: a fill in the last block before it is
/// seen first, and the chain's clock may trail the canister's.
pub const RESCUE_MARGIN_S: u64 = 300;

/// The proof's fee is asked of the prover and paid with this much more (in percent): the
/// prover refunds what is over.
pub const FEE_MARGIN_PERCENT: u64 = 150;

/// The source transaction of an order the canister knows: a relayed order's (its flow, keyed
/// by the order id), a pool payout's whose pinned order it is, a move's whose pinned order it
/// is. None for an order the canister never signed for.
pub fn source_of<S: Store>(state: &State<S>, order_id: [u8; 32]) -> Option<(ChainId, TxHash)> {
    if let Some(flow) = state.flow(&QuoteHash::new(order_id)) {
        if let (Some(chain), Some(tx)) = (flow.chain_id, flow.tx_hash) {
            return Some((chain, tx));
        }
    }
    let config = config::get();
    let pool_chain = config.pool_chain?;
    let payout = state
        .store()
        .pool_payouts()
        .into_iter()
        .find_map(|(_, payout)| {
            let pinned = payout.order.as_ref()?;
            (pinned.order_id == order_id).then_some(payout.tx_hash?)
        });
    if let Some(tx) = payout {
        return Some((pool_chain, tx));
    }
    state
        .store()
        .pool_moves()
        .into_iter()
        .find_map(|(_, item)| {
            let pinned = item.order.as_ref()?;
            if pinned.order_id != order_id {
                return None;
            }
            let chain = match item.direction {
                MoveDirection::BaseToIcp => pool_chain,
                MoveDirection::IcpToBase => ChainId::ETHEREUM,
            };
            Some((chain, item.tx_hash?))
        })
}

/// The intent `receipt` published on Eco's Portal, if it published one.
pub fn published(receipt: &Receipt, portal: EvmAddress) -> Option<PublishedIntent> {
    let mut intents = receipt
        .logs
        .iter()
        .filter(|log| log.address == portal)
        .filter_map(|log| swapic::read_intent_published(&log.topics, &log.data));
    let intent = intents.next()?;
    // two would name no one intent
    intents.next().is_none().then_some(intent)
}

/// What a rescue signs: where, the intent, and the call less the proof's fee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescue {
    pub destination: ChainId,
    pub intent: [u8; 32],
    pub reward_hash: [u8; 32],
    pub prover: EvmAddress,
    pub route: Vec<u8>,
}

/// The rescue of the intent `published` on `source` at `now_s`, pure: its hash is the one its
/// route and reward make, it names Eco's prover, and its route's deadline passed.
pub fn plan(
    published: &PublishedIntent,
    prover: Option<EvmAddress>,
    now_s: u64,
) -> Result<Rescue, String> {
    let reward = Reward {
        deadline: published.reward_deadline,
        creator: published.creator,
        prover: published.prover,
        native: published.reward_native,
        tokens: published.reward_tokens.clone(),
    };
    let reward_hash = eco::reward_hash(&reward);
    let intent = eco::intent_hash(published.destination, &published.route, reward_hash);
    if intent != published.intent_hash {
        return Err("the published intent's hash is not its route's and reward's".to_string());
    }
    match prover {
        Some(prover) if prover == published.prover => {}
        Some(_) => return Err("the intent names another prover than Eco's".to_string()),
        None => return Err("no Eco prover is configured".to_string()),
    }
    let deadline = eco::route_deadline(&published.route).ok_or("the route does not read")?;
    if now_s <= deadline.saturating_add(RESCUE_MARGIN_S) {
        return Err(format!(
            "the intent may still be filled until {deadline}, and is rescued {RESCUE_MARGIN_S} s after"
        ));
    }
    Ok(Rescue {
        destination: ChainId::new(published.destination),
        intent,
        reward_hash,
        prover: published.prover,
        route: published.route.clone(),
    })
}

fn word_of(answer: crate::evm_reads::Simulated) -> Result<[u8; 32], String> {
    match answer {
        crate::evm_reads::Simulated::Returned(bytes) if bytes.len() == 32 => {
            Ok(bytes.try_into().expect("BUG: checked to be 32 bytes"))
        }
        _ => Err("the prover answered no fee".to_string()),
    }
}

/// `rescue_eco_intent`'s body: the signed transaction's id in `signed_txs`.
pub async fn rescue(order_id: Vec<u8>) -> Result<u64, String> {
    require_watcher().map_err(|error| format!("{error:?}"))?;
    require_not_halted().map_err(|error| format!("{error:?}"))?;
    let order_id: [u8; 32] = order_id
        .try_into()
        .map_err(|_| "the order id is not 32 bytes".to_string())?;
    let rescued = || read_state(|state| state.store().eco_rescue(&PoolRef::new(order_id)));
    if let Some(index) = rescued() {
        return Ok(index);
    }
    let (source, tx) = read_state(|state| source_of(state, order_id))
        .ok_or("the canister signed nothing for that order")?;
    let config = config::get();
    let portal = config.eco_portal.ok_or("no Eco Portal is configured")?;
    let receipt = read_source(source, tx)
        .await
        .map_err(|error| format!("the order's transaction: {error:?}"))?;
    if !receipt.success {
        return Err("the order's transaction reverted: it published nothing".to_string());
    }
    let published = published(&receipt, portal).ok_or("the order published no Eco intent")?;
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    let rescue = plan(&published, flow_settings::get().eco_prover, now_s)?;
    let destination = config
        .evm_chains
        .get(rescue.destination)
        .ok_or_else(|| format!("chain {} signs nothing", rescue.destination))?;
    let deadline = eco::route_deadline(&rescue.route).ok_or("the route does not read")?;
    match crate::pool::eco::fill(rescue.destination, rescue.intent, deadline).await? {
        Fill::Open => {}
        Fill::Cancelled => return Err("the intent was cancelled already".to_string()),
        Fill::Filled => return Err("the intent was filled: nothing to rescue".to_string()),
    }
    if crate::pool::eco::reward_status(source, rescue.intent).await? == eco::REWARD_REFUNDED {
        return Err("the intent's reward was refunded already".to_string());
    }
    let canister = crate::storage::ecdsa_address::get().ok_or("no canister address yet")?;
    let data = eco::prove_data(rescue.prover);
    let proofs = eco::cancel_proofs(rescue.destination.get(), rescue.intent);
    let fee = crate::evm_reads::eth_call(
        rescue.destination,
        canister,
        rescue.prover,
        &eco::fetch_fee_calldata(source.get(), &proofs, &data),
    )
    .await
    .map_err(|error| format!("the proof's fee: {:?}", error.kind()))?;
    let fee = Wei::from_be_bytes(word_of(fee)?);
    let value = fee
        .checked_mul(FEE_MARGIN_PERCENT)
        .and_then(|scaled| scaled.checked_div_ceil(100_u64))
        .ok_or("the proof's fee overflows")?;
    // the proof's fee is held to the rescue's own budget, as its gas is
    let budget = destination
        .gas_budget
        .get(GasAction::EcoRescue)
        .ok_or_else(|| format!("chain {} has no eco_rescue row", rescue.destination))?;
    if value > budget {
        return Err(format!(
            "the proof costs {value} wei, above the rescue's budget of {budget}"
        ));
    }
    let gas = destination
        .gas_limit
        .get(GasAction::EcoRescue)
        .ok_or_else(|| format!("chain {} has no eco_rescue row", rescue.destination))?;
    let call = eco::cancel_and_prove_calldata(
        rescue.intent,
        &rescue.route,
        rescue.reward_hash,
        rescue.prover,
        source.get(),
        &data,
    )
    .ok_or("the route does not read")?;
    // the awaits are behind: rescued meanwhile, it answers that one
    if let Some(index) = rescued() {
        return Ok(index);
    }
    let signed = tx::create_and_send(
        TxPurpose::EcoRescue(order_id),
        rescue.destination,
        portal,
        value,
        call,
        gas,
        GasAction::EcoRescue,
    )
    .await
    .map_err(|error| error.to_string())?;
    crate::pool::log(&format!(
        "rescue: order 0x{}'s intent 0x{} on chain {}, its proof's fee {value} wei: cancelAndProve signed ({})",
        hex::encode(order_id),
        hex::encode(rescue.intent),
        rescue.destination,
        signed.id.get()
    ));
    Ok(signed.id.get())
}
