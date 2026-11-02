//! An ICP sweep: the pool's ckUSDC fee income, the canister's ckETH or its ICP. Its line
//! (`Swept`) is written before the transfer is asked, in the message that checked it; a
//! refusal takes it back, and a lost answer is asked again, the same transfer, by the
//! hourly pass until the ledger answers it.

use crate::guards::{require_controller, require_not_halted};
use crate::ic::{self, Account, CallError, Dedup};
use crate::state::Store;
use crate::storage::events::{append_event, read_state};
use crate::storage::timer_state;
use candid::{Nat, Principal};
use settlement_api::types::sweep::{SweepError, SweepIcpArgs, SweepIcpOk};
use types::events::TxPurpose;
use types::pool::IcpAccount;
use types::sweep::{IcpSweepAsk, Sweep, SweepAsset, SweepTo};
use types::{EventType, Timestamp, TokenAmount};

/// The ledger an ICP sweep of `asset` is asked of.
fn ledger_of(asset: SweepAsset) -> Result<Principal, SweepError> {
    let unset = |what: &str| SweepError::Invalid(format!("no {what} is configured"));
    let settings = crate::storage::pool::ledger().settings;
    match asset {
        SweepAsset::CkUsdcFees => settings
            .map(|settings| settings.ckusdc_ledger)
            .ok_or_else(|| unset("pool")),
        SweepAsset::CkEth => crate::storage::flow_settings::get()
            .cketh_ledger
            .ok_or_else(|| unset("ckETH ledger")),
        SweepAsset::Icp => settings
            .and_then(|settings| settings.routes)
            .and_then(|routes| routes.icp_ledger)
            .map(|ledger| ledger.0)
            .ok_or_else(|| unset("ICP ledger")),
        SweepAsset::Native | SweepAsset::FeeUsdc => {
            Err(SweepError::Invalid("not an ICP asset".to_string()))
        }
    }
}

/// The memo of sweep `id`'s transfer: its transaction key, so a retry is the same transfer.
fn memo_of(id: u64) -> [u8; 32] {
    TxPurpose::Sweep(id)
        .quote_hash()
        .expect("BUG: a sweep names its key")
        .into_bytes()
}

fn nat_of(amount: TokenAmount) -> Nat {
    Nat::from(amount)
}

/// `sweep_icp`'s body.
pub async fn sweep(args: SweepIcpArgs) -> Result<SweepIcpOk, SweepError> {
    require_controller().map_err(SweepError::Guard)?;
    require_not_halted().map_err(SweepError::Guard)?;
    let invalid = |why: &str| SweepError::Invalid(why.to_string());
    let asset = SweepAsset::from(args.asset);
    let amount = TokenAmount::from_canonical_nat(args.amount.clone())
        .filter(|amount| amount.try_into_u128().is_some())
        .ok_or_else(|| invalid("amount is wider than 128 bits"))?;
    if amount == TokenAmount::ZERO {
        return Err(invalid("amount must be above zero"));
    }
    let to = IcpAccount::try_from(args.to.clone()).map_err(|error| invalid(&error.to_string()))?;
    let canister = ic_cdk::api::id();
    if to.owner == canister {
        return Err(invalid("to is the canister's own account"));
    }
    if crate::pool::principal_sanctioned(to.owner, &crate::storage::sanctions::is_sanctioned) {
        return Err(SweepError::Sanctioned);
    }
    let ledger = ledger_of(asset)?;
    let unavailable = |error: CallError| SweepError::Unavailable(error.to_string());
    let fee = ic::icrc1_fee(ledger).await.map_err(unavailable)?;
    let fee = TokenAmount::from_canonical_nat(fee).ok_or_else(|| invalid("the ledger's fee"))?;
    let total = amount.checked_add(fee).ok_or_else(|| invalid("amount"))?;
    if asset != SweepAsset::CkUsdcFees {
        let balance = ic::icrc1_balance_of(ledger, Account::of(canister, None))
            .await
            .map_err(unavailable)?;
        let balance = TokenAmount::from_canonical_nat(balance).unwrap_or(TokenAmount::ZERO);
        if total > balance {
            return Err(SweepError::AboveBooks {
                available: nat_of(balance.checked_sub(fee).unwrap_or(TokenAmount::ZERO)),
            });
        }
    }
    // from here to the line no await: what is checked is what the line is written on
    require_not_halted().map_err(SweepError::Guard)?;
    let (id, from_pool_fees) = read_state(|state| -> Result<_, SweepError> {
        if let Some(why) = super::busy(state, asset) {
            return Err(SweepError::Busy(why));
        }
        let from_pool_fees = if asset == SweepAsset::CkUsdcFees {
            let spare = super::spare_fees_icp(state);
            if total > spare {
                return Err(SweepError::AboveBooks {
                    available: nat_of(spare.checked_sub(fee).unwrap_or(TokenAmount::ZERO)),
                });
            }
            total
        } else {
            TokenAmount::ZERO
        };
        Ok((state.store().sweep_count(), from_pool_fees))
    })?;
    let sweep = Sweep {
        id,
        asset,
        to: SweepTo::Icp(to),
        amount,
        from_pool_fees,
        from_fee_income: TokenAmount::ZERO,
    };
    append_event(EventType::Swept { sweep })
        .map_err(|error| SweepError::Unavailable(format!("the sweep's line: {error}")))?;
    let ask = IcpSweepAsk {
        asset,
        to,
        amount,
        created_at: Timestamp::from_nanos(ic_cdk::api::time()),
    };
    timer_state::update(|state| {
        state.icp_sweeps.insert(id, ask);
        ((), true)
    });
    let block = transfer(id, ask, ledger).await?;
    Ok(SweepIcpOk {
        id,
        block: Nat::from(block),
    })
}

/// Asks sweep `id`'s transfer of `ledger`: its block once landed (a duplicate's included); a
/// refusal takes the sweep back; a lost answer keeps it to be asked again; an answer the
/// ledger can no longer deduplicate stops the asking, for a human to look up by its memo.
async fn transfer(id: u64, ask: IcpSweepAsk, ledger: Principal) -> Result<u64, SweepError> {
    let to = Account {
        owner: ask.to.owner,
        subaccount: ask.to.subaccount.map(|bytes| bytes.to_vec()),
    };
    let dedup = Dedup {
        memo: memo_of(id),
        created_at_nanos: ask.created_at.as_nanos(),
    };
    let answer = ic::icrc1_transfer(ledger, None, to, nat_of(ask.amount), dedup).await;
    let done = || {
        timer_state::update(|state| {
            let had = state.icp_sweeps.remove(&id).is_some();
            ((), had)
        })
    };
    match answer {
        Ok(block) => {
            done();
            Ok(block)
        }
        Err(CallError::Refused(why)) => {
            done();
            super::evm::taken_back(id, &why, Timestamp::from_nanos(ic_cdk::api::time()));
            Err(SweepError::Ledger(why))
        }
        Err(CallError::Unknown(why)) => Err(SweepError::Unavailable(format!(
            "{why}; the canister asks again with the same transfer"
        ))),
        Err(CallError::Expired(why)) => {
            done();
            crate::pool::log(&format!(
                "ALERT: the sweep {id}'s transfer is past the ledger's deduplication window \
                 unanswered ({why}): look it up by its memo"
            ));
            Err(SweepError::Unavailable(why))
        }
    }
}

/// The hourly pass's retry of every ICP sweep whose answer was lost.
pub async fn retry_lost() {
    for (id, ask) in timer_state::get().icp_sweeps {
        let Ok(ledger) = ledger_of(ask.asset) else {
            continue;
        };
        if let Err(error) = transfer(id, ask, ledger).await {
            crate::pool::log(&format!("the sweep {id}'s transfer: {error:?}"));
        }
    }
}
