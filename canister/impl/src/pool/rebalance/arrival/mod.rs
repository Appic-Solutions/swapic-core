//! A move's arrival on Base (interface section 3d): `note_move_arrival(id, base_tx, src_chain,
//! src_tx)`, and the same rule inside the canister's own read of Base. The canister reads the
//! Base receipt itself (and an Eco arrival's source receipt) and proves the transfer to its
//! own address on the rails' own logs (`pool::arrival::find`): a CCTP mint whose received
//! message carries the move's own ref in its envelope, or an Eco fill whose source
//! transaction started the order under the move's ref. The move is credited once, under the
//! transfer's credit key, at most its amount: the same arrival reported twice, by the watcher
//! and by the canister's own read, is credited once.
//!
//! The Eco refund of a Base to ICP move's unfilled intent, to the canister's Base address,
//! routes the move again (`MoveRefunded`).

#[cfg(test)]
mod tests;

use crate::pool::arrival::{self, Found, Proof};
use crate::pool::{self, now};
use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use settlement_api::types::pool::{bytes32, PoolArrivalOutcome, PoolError};
use types::pool::{move_ref, MoveDirection, MoveStatus, PoolStep};
use types::{ChainId, TxHash};

/// What a transfer to the canister on Base proves for the moves: the arrival of an ICP to
/// Base move on its way under its own ref, or the Eco refund of a Base to ICP move's intent
/// for its whole amount. None for any other transfer, which is the pool's deposits' to judge.
/// A move whose own transaction is not closed yet is not credited: its arrival waits for it.
///
/// The ref is public and anyone's order may carry it: so a move is credited only by a
/// transfer of at least all it may carry (its amount less its route's most: what its own
/// CCTP mint or Eco fill always brings), and a smaller one under its ref is nobody's credit.
/// A stranger who pays that much in under the ref pays the pool in full, and the move's own
/// arrival after it is then held for a controller like any transfer no open move takes.
pub fn step_for<S: Store>(state: &State<S>, found: &Found) -> Option<PoolStep> {
    let open = super::open_moves(state);
    match &found.proof {
        Proof::Deposit { r#ref, .. } => {
            let item = open.into_iter().find(|item| {
                item.direction == MoveDirection::IcpToBase && move_ref(item.id) == *r#ref
            })?;
            let whole = found.amount >= item.creditable();
            (item.status == MoveStatus::Burned && whole).then(|| PoolStep::MoveArrived {
                id: item.id,
                credited: found.amount.min(item.amount),
                credit: Some(found.credit),
                block: Some(found.block),
                ledger_block: None,
            })
        }
        Proof::Refund { intents } => {
            let item = open.into_iter().find(|item| {
                item.direction == MoveDirection::BaseToIcp
                    && item.status == MoveStatus::Burned
                    && item.landed.is_none()
                    && item.intent.is_some_and(|intent| intents.contains(&intent))
                    && item.amount == found.amount
            })?;
            Some(PoolStep::MoveRefunded {
                id: item.id,
                credit: found.credit,
                amount: found.amount,
                block: found.block,
            })
        }
        _ => None,
    }
}

/// Whether `found` names a move the canister's own read of Base must leave alone: an open
/// ICP to Base move's ref that `step_for` does not credit (its own transaction not closed
/// yet, or less than all it may carry). Kept out of the deposits' rule, so the move's own
/// arrival never becomes an orphan before its move closes.
pub fn waits_for_its_move<S: Store>(state: &State<S>, found: &Found) -> bool {
    let Proof::Deposit { r#ref, .. } = &found.proof else {
        return false;
    };
    super::open_moves(state)
        .into_iter()
        .any(|item| item.direction == MoveDirection::IcpToBase && move_ref(item.id) == *r#ref)
}

/// `note_move_arrival`'s body.
pub async fn note(
    id: u64,
    base_tx: Vec<u8>,
    src_chain: u64,
    src_tx: Vec<u8>,
) -> Result<PoolArrivalOutcome, PoolError> {
    pool::require_running()?;
    let base_tx = TxHash::new(bytes32("base_tx", &base_tx).map_err(PoolError::from)?);
    // the cost guard (P11e): a caller other than the watcher, before any outcall
    pool::report_guard::admit(base_tx, now())?;
    let source = if src_tx.is_empty() {
        None
    } else {
        let tx = bytes32("src_tx", &src_tx).map_err(PoolError::from)?;
        Some((ChainId::new(src_chain), TxHash::new(tx)))
    };
    let item = super::item(id).ok_or(PoolError::Unknown)?;
    if item.direction != MoveDirection::IcpToBase {
        return Err(PoolError::BadArrival(
            "a move to ICP arrives in the rebalance subaccount, not on Base".to_string(),
        ));
    }
    match item.status {
        MoveStatus::Burned => {}
        MoveStatus::Arrived => return Err(PoolError::AlreadyCredited),
        MoveStatus::Lost(_) => {
            return Err(PoolError::BadArrival(
                "the move was written off".to_string(),
            ))
        }
        _ => {
            return Err(PoolError::Unavailable(
                "the move's own transaction is not final yet: ask again".to_string(),
            ))
        }
    }
    let found = arrival::read_found(base_tx, source).await?;
    // after the last read: the credited set and the append are in this one message
    pool::require_running()?;
    let now = now();
    let mut credited_already = false;
    for one in &found {
        if read_state(|state| state.store().is_pool_credited(&one.credit)) {
            credited_already |=
                matches!(&one.proof, Proof::Deposit { r#ref, .. } if *r#ref == move_ref(id));
            continue;
        }
        let Some(step) = read_state(|state| step_for(state, one)) else {
            continue;
        };
        let PoolStep::MoveArrived {
            id: arrived,
            credited,
            ..
        } = step
        else {
            continue;
        };
        if arrived != id {
            continue;
        }
        pool_store::record_at(step, now)
            .map_err(|error| PoolError::BadArrival(error.to_string()))?;
        return Ok(PoolArrivalOutcome::Credited {
            r#ref: move_ref(id).into_bytes().to_vec(),
            amount: candid::Nat::from(credited),
        });
    }
    if credited_already {
        return Err(PoolError::AlreadyCredited);
    }
    Err(PoolError::BadArrival(
        "the transaction carries no transfer of this move to the canister".to_string(),
    ))
}
