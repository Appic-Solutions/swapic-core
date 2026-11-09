//! Across in the pool (BSC design 6.2.7 to 6.2.9), every rule pure so each is a unit test:
//!
//! - an ICP payout's deposit, read off its paid receipt: the one `FundsDeposited` of the pool
//!   chain's SpokePool pin whose depositor is the canister (none or two: a human's);
//! - the range of destination blocks a deposit's `FilledRelay` could be in, before any undo of
//!   one read unfilled (attack M5);
//! - a transfer to the canister on the pool chain explained by Across's own events: a fill,
//!   paired with the closest `FilledRelay` before it by the relay's identity (attack H1), or a
//!   refund to the canister (a relayer refund leaf, or a claim the canister made itself:
//!   attack M3);
//! - a fill's origin proof on the source receipt: the order's `OrderStarted`, the deposit
//!   with every relay field the fill repeats, and the Runner's funding of it, a gift in the
//!   SwapProxy tolerated (attack M2);
//! - an Across refund's credit (D9): the longest oldest-first prefix of the payouts owed back
//!   that fits in it, the rest the canister's own surplus.

pub mod claim;
#[cfg(test)]
mod tests;

use crate::pool::arrival::{Log, Proof, Receipt};
use alloy_primitives::U256;
use types::config::AcrossPins;
use types::hash::PoolRef;
use types::pool::payout::{AcrossOut, EvmPayout};
use types::swapic::across::{self, topics as across_topics, FillType, Filled};
use types::swapic::{self, topics};
use types::{BlockNumber, ChainId, EvmAddress, TokenAmount};

/// An address as the SpokePools hold it: a word, its top 12 bytes zero.
pub fn word(address: EvmAddress) -> [u8; 32] {
    address.to_word()
}

/// What a paid receipt shows of the canister's own Across deposit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepositRead {
    One(Box<AcrossOut>),
    /// No deposit of the canister's from the pinned SpokePool.
    None,
    /// More than one: none names the payout's.
    Many,
}

/// The canister's one Across deposit in `logs` (BSC design 6.2.7): a `FundsDeposited` of
/// `spoke_pool`, the pool chain's pin, whose depositor is the canister; its time the log's
/// block time, or its quote's when the receipt gives none.
pub fn deposit_in(
    logs: &[Log],
    spoke_pool: EvmAddress,
    canister: EvmAddress,
    origin: ChainId,
) -> DepositRead {
    let mut found = logs
        .iter()
        .filter(|log| log.is(spoke_pool, across_topics::funds_deposited()))
        .filter_map(|log| {
            let deposited = across::read_funds_deposited(&log.topics, &log.data, origin)?;
            (deposited.relay.depositor == word(canister)).then_some((log, deposited))
        });
    let Some((log, deposited)) = found.next() else {
        return DepositRead::None;
    };
    if found.next().is_some() {
        return DepositRead::Many;
    }
    DepositRead::One(Box::new(AcrossOut {
        destination: ChainId::new(deposited.destination),
        relay: deposited.relay,
        deposit_time: log
            .block_time
            .unwrap_or(u64::from(deposited.quote_timestamp)),
    }))
}

/// How far back of a block a fill could be: blocks are counted at half the chain's block
/// time, and this many more, so a block time that shrank since the config was written still
/// leaves the deposit's block inside the range.
pub const LOG_MARGIN_BLOCKS: u64 = 100;

/// The most blocks one `eth_getLogs` page reads.
pub const LOG_PAGE_BLOCKS: u64 = 500;

/// The most pages one read of a fill's logs asks for.
pub const MAX_LOG_PAGES: u64 = 64;

/// The pages, `[from, to]` each, of the destination blocks a deposit made at
/// `deposit_time` could have been filled in, up to `head` whose time is `head_time`: back
/// from `head` by twice the blocks the time between them spans at `block_ms`, and
/// [`LOG_MARGIN_BLOCKS`]. None when that is more than [`MAX_LOG_PAGES`] pages: a read that
/// late is a human's.
pub fn log_pages(
    head: BlockNumber,
    head_time: u64,
    deposit_time: u64,
    block_ms: u64,
) -> Option<Vec<(u64, u64)>> {
    let span_ms = head_time.saturating_sub(deposit_time).saturating_mul(1_000);
    let back = span_ms
        .saturating_mul(2)
        .checked_div(block_ms.max(1))?
        .saturating_add(LOG_MARGIN_BLOCKS);
    let to = head.get();
    let from = to.saturating_sub(back);
    let pages = (to - from) / LOG_PAGE_BLOCKS + 1;
    if pages > MAX_LOG_PAGES {
        return None;
    }
    Some(
        (0..pages)
            .map(|page| {
                let start = from + page * LOG_PAGE_BLOCKS;
                (start, to.min(start + LOG_PAGE_BLOCKS - 1))
            })
            .collect(),
    )
}

/// What the pool chain's own configuration says of Across there: its SpokePool pin and USDC,
/// the canister, and the chains a deposit into the pool may come from (each with Across's
/// pins, never the pool chain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcrossPool {
    pub chain: ChainId,
    pub spoke_pool: EvmAddress,
    pub usdc: EvmAddress,
    pub canister: EvmAddress,
    pub origins: Vec<ChainId>,
}

/// What explains one transfer to the canister among Across's events of the same receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Explained {
    /// A relayer's (or a slow) fill of a deposit to the canister: its `FilledRelay`.
    Fill(Box<Filled>),
    /// A fill is the nearest explanation, and it does not match the transfer: held.
    Mismatch(&'static str),
    /// Across refunded the canister: a relayer refund leaf, or its own claim.
    Refund,
    /// Nothing of Across's: the rails before it read the transfer.
    Nothing,
}

/// The transfers of `token` to `to` in `logs`: index, sender, value.
fn transfers(logs: &[Log], token: EvmAddress, to: EvmAddress) -> Vec<(u64, EvmAddress, U256)> {
    logs.iter()
        .filter(|log| log.is(token, topics::transfer()) && log.topics.len() == 3)
        .filter(|log| log.topics[2] == word(to))
        .filter_map(|log| {
            let from = swapic::evm_of_word(log.topics[1])?;
            let value: [u8; 32] = log.data.as_slice().try_into().ok()?;
            Some((log.index, from, U256::from_be_bytes(value)))
        })
        .collect()
}

/// What explains the canister's transfer at `index` from `from` of `value` in `logs`
/// (attack H1, M3, 10.3 #19): the closest `FilledRelay` of the SpokePool pin before it, with
/// no other transfer to the canister between them, when the pair is one fill (its relayer the
/// sender, or the SpokePool for a slow fill; the canister its first and its updated recipient;
/// no message; the pool chain's USDC; exactly what arrived; from a chain a deposit may come
/// from); else, for a transfer from the SpokePool, a refund leaf or the canister's own claim
/// paying it; else nothing.
pub fn explain(
    logs: &[Log],
    index: u64,
    from: EvmAddress,
    value: TokenAmount,
    pool: &AcrossPool,
    used_refunds: &mut Vec<(u64, usize)>,
) -> Explained {
    let canister_transfers = transfers(logs, pool.usdc, pool.canister);
    let fill = logs
        .iter()
        .filter(|log| log.index < index && log.is(pool.spoke_pool, across_topics::filled_relay()))
        .max_by_key(|log| log.index);
    if let Some(fill_log) = fill {
        let between = canister_transfers
            .iter()
            .any(|(at, _, _)| *at > fill_log.index && *at < index);
        if !between {
            return match across::read_filled_relay(&fill_log.topics, &fill_log.data) {
                Some(filled) => match fill_matches(&filled, from, value, pool) {
                    Ok(()) => Explained::Fill(Box::new(filled)),
                    Err(why) => Explained::Mismatch(why),
                },
                None => Explained::Mismatch("the fill before it does not read"),
            };
        }
    }
    if from == pool.spoke_pool && refunded(logs, value, pool, used_refunds) {
        return Explained::Refund;
    }
    Explained::Nothing
}

/// Whether `filled` is the fill that sent `value` from `from` to the canister.
fn fill_matches(
    filled: &Filled,
    from: EvmAddress,
    value: TokenAmount,
    pool: &AcrossPool,
) -> Result<(), &'static str> {
    let by = match filled.fill_type {
        FillType::SlowFill => from == pool.spoke_pool,
        FillType::FastFill | FillType::ReplacedSlowFill => filled.relayer == word(from),
    };
    if !by {
        return Err("the fill's relayer did not send it");
    }
    if filled.key.recipient != word(pool.canister)
        || filled.updated_recipient != word(pool.canister)
    {
        return Err("the fill is not to the canister");
    }
    if filled.key.message_hash != [0; 32] || filled.updated_message_hash != [0; 32] {
        return Err("the fill carries a message");
    }
    if filled.key.output_token != word(pool.usdc) {
        return Err("the fill pays another token");
    }
    if filled.updated_output_amount != value.to_u256() {
        return Err("the fill pays another amount");
    }
    let origin = ChainId::new(filled.key.origin_chain_id);
    if origin == pool.chain || !pool.origins.contains(&origin) {
        return Err("the fill's deposit is from no chain the canister takes Across deposits from");
    }
    Ok(())
}

/// Whether a refund in `logs` pays the canister `value` from the SpokePool pin: an entry of a
/// relayer refund leaf on the pool chain, in its USDC, paid now (not deferred), naming the
/// canister for exactly `value`; or a claim of the canister's deferred refunds for exactly
/// `value` that the canister itself made (anyone may claim their own balance to any address:
/// attack M3). Each leaf entry pays one transfer: `used` holds the ones taken.
fn refunded(
    logs: &[Log],
    value: TokenAmount,
    pool: &AcrossPool,
    used: &mut Vec<(u64, usize)>,
) -> bool {
    let leaf = logs
        .iter()
        .filter(|log| {
            log.is(
                pool.spoke_pool,
                across_topics::executed_relayer_refund_root(),
            )
        })
        .find_map(|log| {
            let leaf = across::read_refund_root(&log.topics, &log.data)?;
            if leaf.chain_id != pool.chain.get() || leaf.token != pool.usdc || leaf.deferred {
                return None;
            }
            leaf.refunds
                .iter()
                .enumerate()
                .find(|(at, (to, amount))| {
                    *to == pool.canister && *amount == value && !used.contains(&(log.index, *at))
                })
                .map(|(at, _)| (log.index, at))
        });
    if let Some(entry) = leaf {
        used.push(entry);
        return true;
    }
    let claim = logs
        .iter()
        .filter(|log| log.is(pool.spoke_pool, across_topics::claimed_relayer_refund()))
        .filter(|log| !used.contains(&(log.index, 0)))
        .find(|log| {
            across::read_claimed_refund(&log.topics, &log.data).is_some_and(|claimed| {
                claimed.token == word(pool.usdc)
                    && claimed.refund_address == word(pool.canister)
                    && claimed.caller == pool.canister
                    && claimed.amount == value
            })
        });
    if let Some(log) = claim {
        used.push((log.index, 0));
        return true;
    }
    false
}

/// The source chain's configuration an origin proof reads: its Swapic, its Runner and its
/// Across pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcrossSource {
    pub chain: ChainId,
    pub swapic: EvmAddress,
    pub runner: EvmAddress,
    pub pins: AcrossPins,
}

/// The deposit `filled` fills, proven on its source receipt (BSC design 6.2.8): exactly one
/// `OrderStarted` of the source's Swapic; exactly one `FundsDeposited` of the source's
/// SpokePool pin with the fill's deposit id, for the pool chain, to the canister, in the pool
/// chain's USDC, its depositor the order's payer, no message, and every relay field the fill
/// repeats; and the Runner funded it: the periphery's one `SwapBeforeBridge` of that input,
/// the Runner's transfer to the periphery of what it pulled, and the periphery's transfer to
/// the SpokePool of the whole deposit, at least that (a gift left in the SwapProxy is
/// tolerated). The deposit under the order's ref, its payer bound; else why it is not.
pub fn origin_proof(
    filled: &Filled,
    source: &Receipt,
    at: &AcrossSource,
    pool: &AcrossPool,
) -> Proof {
    let unproven = |why: &str| Proof::Unproven {
        why: why.to_string(),
    };
    if ChainId::new(filled.key.origin_chain_id) != at.chain {
        return unproven("the source given is not the chain the deposit came from");
    }
    if !source.success {
        return unproven("the source transaction reverted");
    }
    let orders: Vec<(PoolRef, EvmAddress)> = source
        .logs
        .iter()
        .filter(|log| log.is(at.swapic, topics::order_started()))
        .filter_map(|log| swapic::read_order_started(&log.topics, &log.data))
        .map(|order| (PoolRef::new(order.r#ref), order.payer))
        .collect();
    let [(r#ref, payer)] = orders.as_slice() else {
        return unproven("the source transaction holds no single order");
    };
    let deposits: Vec<across::Deposited> = source
        .logs
        .iter()
        .filter(|log| log.is(at.pins.spoke_pool, across_topics::funds_deposited()))
        .filter_map(|log| across::read_funds_deposited(&log.topics, &log.data, at.chain))
        .filter(|deposited| deposited.relay.deposit_id.to_u256() == filled.key.deposit_id)
        .collect();
    let [deposited] = deposits.as_slice() else {
        return unproven("the source transaction holds no single deposit the fill fills");
    };
    let relay = &deposited.relay;
    let bound = deposited.destination == pool.chain.get()
        && relay.recipient == word(pool.canister)
        && relay.output_token == word(pool.usdc)
        && relay.depositor == word(*payer)
        && relay.message.is_empty()
        && relay.key() == filled.key;
    if !bound {
        return unproven("the deposit is not the fill's, to the canister, from the order's payer");
    }
    let Some(input_token) = swapic::evm_of_word(relay.input_token) else {
        return unproven("the deposit's input is no address");
    };
    let swaps: Vec<across::Swapped> = source
        .logs
        .iter()
        .filter(|log| log.is(at.pins.periphery, across_topics::swap_before_bridge()))
        .filter_map(|log| across::read_swap_before_bridge(&log.topics, &log.data))
        .collect();
    let [swapped] = swaps.as_slice() else {
        return unproven("the periphery logged no single swap before the deposit");
    };
    let input = relay.input_amount.to_u256();
    let pulled = swapped.swap_token_amount;
    let funded = swapped.swap_token == input_token
        && swapped.input_token == input_token
        && swapped.input_amount == input
        && input >= pulled
        && transfers(&source.logs, input_token, at.pins.periphery)
            .iter()
            .any(|(_, from, value)| *from == at.runner && *value == pulled)
        && transfers(&source.logs, input_token, at.pins.spoke_pool)
            .iter()
            .any(|(_, from, value)| *from == at.pins.periphery && *value == input);
    if !funded {
        return unproven("the Runner did not fund the deposit");
    }
    Proof::Deposit {
        r#ref: *r#ref,
        payer: Some(*payer),
    }
}

/// The payouts an Across refund of `amount` credits (D9): of `owed` (each payout owed back by
/// Across, its deposit's input and its fill deadline), oldest fill deadline first, then id,
/// the longest prefix whose inputs sum to at most `amount`. None when payouts are owed back
/// and not even the oldest fits: the transfer is held for a controller.
pub fn credited(owed: &[(u64, TokenAmount, u64)], amount: TokenAmount) -> Option<Vec<u64>> {
    let mut sorted = owed.to_vec();
    sorted.sort_by_key(|(id, _, deadline)| (*deadline, *id));
    let mut left = amount;
    let mut prefix = Vec::new();
    for (id, input, _) in sorted {
        match left.checked_sub(input) {
            Some(rest) => {
                left = rest;
                prefix.push(id);
            }
            None => break,
        }
    }
    if prefix.is_empty() && !owed.is_empty() {
        return None;
    }
    Some(prefix)
}

/// The payouts owed back by Across now, as [`credited`] reads them.
pub fn owed_back(payouts: &[EvmPayout]) -> Vec<(u64, TokenAmount, u64)> {
    payouts
        .iter()
        .filter(|payout| payout.reclaim == Some(types::pool::Reclaim::Pending))
        .filter_map(|payout| {
            let out = payout.across.as_ref()?;
            Some((payout.id, out.relay.input_amount, out.fill_deadline()))
        })
        .collect()
}
