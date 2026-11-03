//! What the outbox does with a transaction that is not landing: the same bytes sent again
//! after a while, and a replacement at a higher fee after longer, held to the reading's
//! ceiling and the transaction's gas budget.

use super::*;

/// A transaction that has not landed: the same bytes again while it is young, and a
/// replacement at the same nonce once it is not (A5). Neither abandons the nonce.
///
/// The two clocks are deliberately different. The rebroadcast runs from the last time the
/// bytes went out, because that is what it repairs: a provider that dropped them. The
/// replacement runs from the FIRST time they went out, because a rebroadcast that reset it
/// would postpone the fee bump every thirty seconds and the transaction would sit
/// underpriced forever.
pub(super) async fn push_again(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) {
    let (Some(_), Some(since_first)) = (entry.sent_for(now), entry.out_for(now)) else {
        return;
    };
    if since_first >= STUCK_AFTER {
        // a replacement is a new transaction, signed and broadcast, so the halt switch
        // gates it like every other path that creates one, and a halted canister leaves
        // the entry exactly where it is. Reading receipts and closing attempts carries on
        // while halted: ops investigating a divergence want to see what the
        // chains did with what is already out there.
        if require_not_halted().is_err() {
            return;
        }
        if replace(entry, chain_id).await {
            return;
        }
        // no replacement: already bidding the ceiling, or the reading is too old to price
        // a bid against. The bytes are still the right bytes and the nonce is still this
        // entry's, so they keep going out on the rebroadcast cadence below.
    }
    rebroadcast(entry, now);
}

/// The entry's current bytes queued to go out again once [`REBROADCAST_AFTER`] has passed
/// since they last did: the same bytes, so no new signature and no new line in the log.
/// The one way a closed entry goes out again.
pub(super) fn rebroadcast(entry: &OutboxEntry, now: Timestamp) {
    if entry
        .sent_for(now)
        .is_some_and(|since_last| since_last >= REBROADCAST_AFTER)
    {
        let mut queued = entry.clone();
        queued.status = OutboxStatus::Queued;
        queued.last_sent_at = None;
        outbox::put(queued);
    }
}

/// Re-signs the same transaction at a higher fee and records it, keeping the nonce, and
/// answers whether it did.
///
/// A replacement signs new bytes and broadcasts them, so callers gate it on
/// `require_not_halted`. It answers `false` when the fee cannot be raised: the entry is
/// already bidding the ceiling [`ChainData::fee_ceiling`] allows, or the chain reading is
/// too old to price against, or a call failed. None of those abandon the nonce; the current
/// bytes stay on the network.
pub(super) async fn replace(entry: &OutboxEntry, chain_id: ChainId) -> bool {
    // no call brings a bump a reading: the canister reads the chain's gas itself
    if crate::gas::ensure_fresh(chain_id).await.is_err() {
        return false;
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    // a replacement a node accepts pays at least an eighth more, so the fee doubles: the
    // fresh chain data is a floor, not the answer, because the old fee may already be
    // above it, and the ceiling is what stops the doubling from running away
    let Some(fees) = bumped(entry, chain_id, now) else {
        return false;
    };
    let tx = Eip1559Tx {
        chain_id,
        nonce: entry.nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        gas_limit: entry.gas_limit,
        to: entry.to,
        value: entry.value,
        data: entry.data.clone(),
    };
    let Ok(signature) = ecdsa::sign(tx.signing_hash()).await else {
        return false;
    };
    let signed = tx.signed(signature);
    let tx_hash = signed.hash();
    let raw_tx = signed.into_raw();
    let appended = append_event(EventType::TxReplaced {
        purpose: entry.purpose,
        chain_id,
        nonce: entry.nonce,
        max_fee: fees.max_fee(),
        max_priority_fee: fees.max_priority_fee(),
        tx_hash,
        raw_tx: raw_tx.clone(),
    });
    if appended.is_err() {
        return false;
    }
    // A3: the entry this pass read is the one replaced, and only if it is still the one
    // the outbox holds
    if outbox::get(entry.key()).is_some_and(|current| current.tx_hash() == entry.tx_hash()) {
        outbox::put(entry.replaced(tx_hash, raw_tx, fees.max_fee(), fees.max_priority_fee()));
    }
    true
}

/// The fees a replacement pays: double what the transaction being replaced paid, never
/// below what the chain is asking now, and never above what the freshest reading allows,
/// what [`max_transaction_cost`] buys per unit of this transaction's gas, or what its gas
/// budget buys, whichever is lowest. Nothing at all once the entry is already at that
/// ceiling, when its chain has no budget for it, or when the chain reading is too old to
/// price a bid against.
pub(super) fn bumped(entry: &OutboxEntry, chain_id: ChainId, now: Timestamp) -> Option<Fees> {
    let reading = fresh_reading(chain_id, now).ok()?;
    // the budget the transaction is held to: its own gas action, as `TxCreated` recorded
    // it and the entry keeps; a cancel, which has none, the strictest budget its chain has.
    // A chain that signs nothing bids nothing more
    let budget = Some(replacement_budget(&config::get(), chain_id, entry.action)?);
    let current = Fees::new(entry.max_fee, entry.max_priority_fee)?;
    bump(chain_id, current, &reading, entry.gas_limit, budget)
}

/// The budget a replacement of a transaction created for `action` is held to: that action's
/// own on `chain_id`, whatever its purpose; a cancel's (no action) the strictest its chain
/// has. Nothing on a chain that signs nothing, or has no row for the action.
pub(super) fn replacement_budget(
    config: &types::Config,
    chain_id: ChainId,
    action: Option<GasAction>,
) -> Option<Wei> {
    match action {
        Some(action) => budget_for(config, chain_id, action).ok(),
        None => config
            .evm_chains
            .get(chain_id)
            .map(|chain| chain.gas_budget.strictest()),
    }
}

/// [`bumped`] on what it read. Pure, so the gas rule's hold on a replacement is a unit
/// test.
pub fn bump(
    chain_id: ChainId,
    current: Fees,
    reading: &ChainData,
    gas_limit: GasAmount,
    budget: Option<Wei>,
) -> Option<Fees> {
    let floor = reading.fees()?;
    // the bill is bounded as well as the price: a large gas limit lowers what can be paid
    // per unit, and the bump clamps to that the way it clamps to the fee ceiling, so the
    // bids keep rising in smaller steps up to the bound instead of stopping one doubling
    // short of it, and a bid is refused only once nothing can be raised
    let mut ceiling = reading
        .fee_ceiling()
        .capped(max_fee_per_gas(chain_id))
        .capped(affordable_per_gas(chain_id, gas_limit)?);
    if let Some(budget) = budget {
        ceiling = ceiling.capped(per_gas(budget, gas_limit)?);
    }
    current.bumped(floor, ceiling)
}

/// The most a transaction burning `gas_limit` may pay per unit of gas and stay inside
/// [`max_transaction_cost`] of its chain. Nothing for a gas limit of zero, which no
/// transaction has.
pub(super) fn affordable_per_gas(chain_id: ChainId, gas_limit: GasAmount) -> Option<WeiPerGas> {
    per_gas(max_transaction_cost(chain_id), gas_limit)
}

pub(super) fn per_gas(cost: Wei, gas_limit: GasAmount) -> Option<WeiPerGas> {
    cost.checked_div_floor(gas_limit.into_inner())
        .map(|cost| cost.change_units())
}
