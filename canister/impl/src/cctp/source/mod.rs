//! What a finish's source transaction shows: the canister reads it itself, from one
//! node, before it signs a finish the watcher asks for, rather than take the watcher's word
//! that it paid the canister's fee. The transaction succeeded, the source chain's
//! `MessageTransmitterV2` sent this very message (every field the burn determined: only the
//! nonce, the finality it was attested at, the fee executed and the expiration block are
//! Circle's to fill in), and the source Runner paid the canister fee steps worth at least
//! the destination's least network fee for the arrival the quoter charged for. The
//! canister's own transfers (a move's, a payout's) pay no fee step and are finished only
//! from its own transactions (`own`), so they read nothing here.

#[cfg(test)]
mod tests;

use super::FinishError;
use crate::pool::arrival::{runner_transfers, Receipt, RunnerTransfer};
use crate::rpc::{self, hex0x};
use serde_json::json;
use types::cctp::BurnMessage;
use types::config::EvmChain;
use types::swapic::{self, topics};
use types::{ChainId, EvmAddress, TokenAmount, TxHash};

/// Whether `receipt` is a source `message` may be finished from, pure: it succeeded, the
/// source chain's transmitter sent `message` in it, and the source Runner paid `canister`
/// fee steps worth at least `needed` in it.
pub fn proves(
    receipt: &Receipt,
    message: &BurnMessage,
    source: &EvmChain,
    canister: EvmAddress,
    needed: TokenAmount,
) -> Result<(), FinishError> {
    if !receipt.success {
        return Err(FinishError::Source("reverted"));
    }
    let sent = receipt
        .logs
        .iter()
        .filter(|log| {
            log.address == source.message_transmitter
                && log.topics.first() == Some(&topics::message_sent())
        })
        .filter_map(|log| swapic::read_message_sent(&log.topics, &log.data))
        .filter_map(|bytes| BurnMessage::parse(&bytes).ok())
        .any(|emitted| message.attests(&emitted));
    if !sent {
        return Err(FinishError::Source("did not send this message"));
    }
    let paid = runner_transfers(&receipt.logs, source.usdc, source.runner, canister)
        .iter()
        .filter_map(|transfer| match transfer {
            RunnerTransfer::Fee { amount, .. } => Some(*amount),
            RunnerTransfer::Payout { .. } => None,
        })
        .try_fold(TokenAmount::ZERO, |sum, amount| sum.checked_add(amount))
        .unwrap_or(TokenAmount::MAX);
    if paid == TokenAmount::ZERO {
        return Err(FinishError::Source("paid the canister no fee step"));
    }
    if paid < needed {
        return Err(FinishError::Source(
            "paid the canister less than the arrival's least network fee",
        ));
    }
    Ok(())
}

/// The arrival the quoter charged a burn's fee for, by its envelope: a Swap's completion, a
/// CkDeposit's, or a Deliver's receipt. Never the finish's own action: a release is no
/// arrival anyone charged for.
pub fn charged_action(envelope: &types::swapic::Envelope) -> types::config::GasAction {
    use types::config::GasAction;
    use types::swapic::Action;
    match envelope.action {
        Action::Swap => GasAction::CompleteSwap,
        Action::CkDeposit => GasAction::CompleteCk,
        Action::Deliver => GasAction::RelayDeliver,
    }
}

/// The receipt of `tx` on `chain`, from one node: what [`proves`] reads.
pub async fn read(chain: ChainId, tx: TxHash) -> Result<Receipt, String> {
    let calls = [("eth_getTransactionReceipt", json!([hex0x(tx.as_ref())]))];
    let receipt = rpc::rpc_batch_each(chain, &calls, crate::tx::MAX_RECEIPT_BYTES_PER_ITEM)
        .await
        // the kind only: an outcall's text can carry the provider's url and key
        .map_err(|error| format!("the source transaction's read: {:?}", error.kind()))?
        .pop()
        .ok_or("the source transaction's read answered nothing")?
        .map_err(|error| format!("the source transaction's read: {:?}", error.kind()))?;
    if receipt.is_null() {
        return Err("the source transaction has no receipt yet".to_string());
    }
    Receipt::parse(&receipt)
        .filter(|receipt| receipt.tx == tx)
        .ok_or_else(|| "the source transaction's receipt does not read".to_string())
}
