//! A controller's sweep of what is the canister's own: how much each fee line can spare,
//! what open work blocks an asset with no line, and the rules the fold holds every sweep to.
//! The doors are `updates::sweep`; the EVM and ICP flows are [`evm`] and [`icp`].

#[cfg(test)]
mod tests;

pub mod evm;
pub mod icp;

use crate::state::{State, Store};
use types::books::BooksError;
use types::pool::{OrphanStatus, PayoutKind, PayoutStatus, PendingOp};
use types::sweep::{Sweep, SweepAsset};
use types::TokenAmount;

fn sum(mut amounts: impl Iterator<Item = TokenAmount>) -> TokenAmount {
    amounts
        .try_fold(TokenAmount::ZERO, TokenAmount::checked_add)
        .unwrap_or(TokenAmount::MAX)
}

/// What the Base fee line can spare: `fees_base` less the fee an orphan's refund kept while
/// the refund is out, which goes back to the orphan if the refund fails.
pub fn spare_fees_base<S: Store>(state: &State<S>) -> TokenAmount {
    let refunding = sum(state
        .store()
        .pool_orphans()
        .into_iter()
        .filter_map(|(_, orphan)| match orphan.status {
            OrphanStatus::Refunding { sent, .. } => orphan.amount.checked_sub(sent),
            _ => None,
        }));
    state
        .pool()
        .lines()
        .fees_base
        .checked_sub(refunding)
        .unwrap_or(TokenAmount::ZERO)
}

/// What the ICP fee line can spare: `fees_icp` less every ICP swap's network fee that an
/// undo could still take back (a payout neither paid nor undone, one held for a human
/// included).
pub fn spare_fees_icp<S: Store>(state: &State<S>) -> TokenAmount {
    let refundable = sum(state
        .store()
        .pool_payouts()
        .into_iter()
        .filter(|(_, payout)| {
            matches!(payout.kind, PayoutKind::IcpSwap(_))
                && !matches!(payout.status, PayoutStatus::Paid | PayoutStatus::Undone(_))
        })
        .map(|(_, payout)| payout.icp_fee()));
    state
        .pool()
        .lines()
        .fees_icp
        .checked_sub(refundable)
        .unwrap_or(TokenAmount::ZERO)
}

/// What may hold a user's ICP or ckETH between two legs, held for a human or not: an
/// arrived swap not paid, an ICP withdrawal, an ICP swap's Base payout neither paid nor
/// undone. None when nothing does.
pub fn icp_tokens_held<S: Store>(state: &State<S>) -> Option<String> {
    let ledger = state.pool();
    let swap = ledger.paying.iter().find(|r#ref| {
        state
            .store()
            .pool_deposit(r#ref)
            .is_some_and(|deposit| deposit.terms.kind.is_swap())
    });
    if let Some(r#ref) = swap {
        return Some(format!("the swap {ref} is being paid", ref = r#ref));
    }
    if let Some(id) = ledger
        .pending
        .iter()
        .find_map(|(id, op)| matches!(op, PendingOp::Withdraw { .. }).then_some(id))
    {
        return Some(format!("the ICP withdrawal {id} is open"));
    }
    state
        .store()
        .pool_payouts()
        .into_iter()
        .find(|(_, payout)| {
            // a burned ck withdrawal holds no token and spends no more ckETH, for days at worst
            matches!(payout.kind, PayoutKind::IcpSwap(_))
                && !matches!(
                    payout.status,
                    PayoutStatus::Paid | PayoutStatus::Undone(_) | PayoutStatus::CkWithdrawn
                )
        })
        .map(|(id, _)| format!("the ICP swap payout {id} is open"))
}

/// What open work keeps a sweep of `asset` from going out now (for the assets no line
/// bounds): ckETH while a move is open (its minter withdrawal spends ckETH) or anything may
/// hold it between legs; ICP while anything may hold it between legs.
pub fn busy<S: Store>(state: &State<S>, asset: SweepAsset) -> Option<String> {
    match asset {
        SweepAsset::CkEth => {
            if let Some(id) = state.pool().moves().open.first() {
                return Some(format!("the move {id} is open"));
            }
            icp_tokens_held(state)
        }
        SweepAsset::Icp => icp_tokens_held(state),
        SweepAsset::Native | SweepAsset::FeeUsdc | SweepAsset::CkUsdcFees => None,
    }
}

/// The rules every `Swept` line meets: the next id, an amount above zero, parts that add up
/// to what its asset takes them from, and no line debited past what it can spare.
pub fn admits<S: Store>(state: &State<S>, sweep: &Sweep) -> Result<(), BooksError> {
    let expected = state.store().sweep_count();
    if sweep.id != expected {
        return Err(BooksError::SweepOutOfOrder {
            expected,
            given: sweep.id,
        });
    }
    if sweep.amount == TokenAmount::ZERO {
        return Err(BooksError::Zero);
    }
    let above = |wanted: TokenAmount, available: TokenAmount| {
        (wanted > available).then_some(BooksError::AboveFees { wanted, available })
    };
    match sweep.asset {
        SweepAsset::FeeUsdc => {
            let chain = sweep.chain().ok_or(BooksError::SweepParts)?;
            if sweep.from_pool_fees.checked_add(sweep.from_fee_income) != Some(sweep.amount) {
                return Err(BooksError::SweepParts);
            }
            if let Some(error) = above(sweep.from_fee_income, state.book(&chain).fee_income)
                .or_else(|| above(sweep.from_pool_fees, spare_fees_base(state)))
            {
                return Err(error);
            }
        }
        SweepAsset::CkUsdcFees => {
            if sweep.chain().is_some()
                || sweep.from_fee_income != TokenAmount::ZERO
                || sweep.from_pool_fees <= sweep.amount
            {
                return Err(BooksError::SweepParts);
            }
            if let Some(error) = above(sweep.from_pool_fees, spare_fees_icp(state)) {
                return Err(error);
            }
        }
        SweepAsset::Native | SweepAsset::CkEth | SweepAsset::Icp => {
            let evm = sweep.chain().is_some();
            if evm != (sweep.asset == SweepAsset::Native)
                || sweep.from_fee_income != TokenAmount::ZERO
                || sweep.from_pool_fees != TokenAmount::ZERO
            {
                return Err(BooksError::SweepParts);
            }
        }
    }
    Ok(())
}

/// The rule of a `SweepReverted` line: a sweep that was made and not taken back yet.
pub fn admits_revert<S: Store>(state: &State<S>, id: u64) -> Result<(), BooksError> {
    match state.store().sweep(id) {
        Some(entry) if !entry.reverted => Ok(()),
        _ => Err(BooksError::NoSweep(id)),
    }
}
