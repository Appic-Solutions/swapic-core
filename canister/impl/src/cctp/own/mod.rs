//! The canister's own CCTP transfers (interface section 3d): a rebalance move's burn, and
//! a pool payout's over CCTP. Their orders pay the canister no fee step: the canister pays its
//! own way, so their finish is the canister's to pay as well, and it is finished only when
//! its source transaction is the canister's own, signed for that move or payout and succeeded
//! at depth (the fold indexes it then), and the message is exactly the burn of the order it
//! pinned. A move's finish is signed under its own gas action (`PoolMoveFinish`, the
//! `pool_move_finish` row); a payout's under the arrival's own (Swap or Deliver).
//!
//! A message that names a move (an open move's ref, or a ck deposit for the canister's own
//! principal and rebalance subaccount) from any other source transaction is refused: a
//! stranger's burn under a move's ref is never finished as the canister's.

#[cfg(test)]
mod tests;

use super::{FinishError, Plan};
use crate::state::{State, Store};
use candid::Principal;
use types::cctp::BurnMessage;
use types::config::GasAction;
use types::flow::FinishAction;
use types::pool::rebalance::read_burn;
use types::pool::{ck_word, move_ref, rebalance_subaccount, OwnSource};
use types::swapic::{self, selectors, Action, Envelope};
use types::TxHash;

/// Whether `message` is the burn of `order_bytes` (`abi.encode(Order)`): its one burn step's
/// destination, recipient, caller, token, fee and finality and its envelope, byte for byte,
/// and an amount the Runner could have burned (at least the step's `min`, at most the order's
/// `amountIn`).
pub fn burn_of(order_bytes: &[u8], message: &BurnMessage) -> bool {
    let Some(order) = swapic::decode_order(order_bytes) else {
        return false;
    };
    let mut burns = order
        .steps
        .iter()
        .filter(|step| swapic::selector_of(step) == Some(selectors::DEPOSIT_FOR_BURN_WITH_HOOK));
    let (Some(step), None) = (burns.next(), burns.next()) else {
        return false;
    };
    let Some(call) = read_burn(&step.data) else {
        return false;
    };
    let amount = message.body.amount.to_u256();
    let max_fee = message.body.max_fee.to_u256();
    call.destinationDomain == message.destination_domain
        && call.mintRecipient.0 == message.body.mint_recipient
        && call.destinationCaller.0 == message.destination_caller
        && call.burnToken.into_word().0 == message.body.burn_token
        && call.maxFee == max_fee
        && call.minFinalityThreshold == message.min_finality_threshold
        && call.hookData.as_ref() == message.body.hook_data.as_slice()
        && amount >= step.min
        && amount <= order.amountIn
}

/// Whether `envelope` names a move: an open move's ref, or a ck deposit for the canister's
/// own principal and rebalance subaccount.
pub fn names_a_move<S: Store>(state: &State<S>, envelope: &Envelope, canister: Principal) -> bool {
    let ours = envelope.action == Action::CkDeposit
        && envelope.principal == ck_word(&canister)
        && envelope.subaccount == rebalance_subaccount();
    ours || state
        .pool()
        .moves()
        .open
        .iter()
        .any(|id| move_ref(*id).into_bytes() == envelope.reference)
}

/// The gas action a finish of `message`, from `source_tx`, as `plan` decided it, is signed
/// under: the plan's own for any other transfer, `PoolMoveFinish` for a move's completion or
/// delivery; or the refusal of a message that names a move or a payout and is not its burn.
pub fn finish_action<S: Store>(
    state: &State<S>,
    source_tx: TxHash,
    message: &BurnMessage,
    plan: &Plan,
    canister: Principal,
) -> Result<GasAction, FinishError> {
    match state.store().pool_own_source(&source_tx) {
        Some(OwnSource::Move(id)) => {
            let pinned = state
                .store()
                .pool_move(id)
                .and_then(|item| item.order)
                .ok_or(FinishError::Shape("the move pinned no order"))?;
            if !burn_of(&pinned.order, message) {
                return Err(FinishError::Shape(
                    "the message is not the burn of the move's own order",
                ));
            }
            // a release pays the canister back on the destination, at the release's own row
            Ok(match plan.action {
                FinishAction::Release => plan.gas_action,
                FinishAction::Complete | FinishAction::Receive => GasAction::PoolMoveFinish,
            })
        }
        Some(OwnSource::Payout(id)) => {
            let pinned = state
                .store()
                .pool_payout(id)
                .and_then(|payout| payout.order)
                .ok_or(FinishError::Shape("the payout pinned no order"))?;
            if !burn_of(&pinned.order, message) {
                return Err(FinishError::Shape(
                    "the message is not the burn of the payout's own order",
                ));
            }
            Ok(plan.gas_action)
        }
        None if names_a_move(state, &plan.envelope, canister) => Err(FinishError::Shape(
            "a move's transfer is finished only from the canister's own transaction",
        )),
        None => Ok(plan.gas_action),
    }
}
