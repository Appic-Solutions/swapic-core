//! The ICP doors' pull, made before anything is written (audit C15; Jeff, 2026-11-08: never a
//! line before the money). `swap_from_icp`, `swap_from_icp_ck` and `stake_icp` check
//! everything first, then pull under a pair of their own; a refused pull writes nothing, a
//! pull whose outcome nobody can tell is kept as a pull to check (the watcher alerts it, a
//! human checks the ledger by its memo), and only money that came in is written down. Money
//! the lines then cannot take is given back at once.

use crate::ic::{self, Account, Dedup, FreshPull};
use crate::storage::timer_state;
use alloy_primitives::keccak256;
use candid::{Nat, Principal};
use settlement_api::types::pool::PoolError;
use types::pool::{IcpAccount, PrincipalBytes};
use types::timer_state::{PullToCheck, MAX_PULLS_TO_CHECK};
use types::{Timestamp, TokenAmount};

/// The pull's pair: the caller's own request at this instant. Two identical requests in one
/// instant share it, and the second is refused by the ledger as a duplicate, never paid.
pub fn dedup(
    door: &str,
    from: &IcpAccount,
    ledger: Principal,
    amount: TokenAmount,
    at: Timestamp,
) -> Dedup {
    let mut preimage = b"swapic.pool.v1.pull-first.".to_vec();
    preimage.extend_from_slice(door.as_bytes());
    preimage.extend_from_slice(from.owner.as_slice());
    preimage.extend_from_slice(&from.subaccount.unwrap_or([0; 32]));
    preimage.extend_from_slice(ledger.as_slice());
    preimage.extend_from_slice(&amount.to_be_bytes());
    preimage.extend_from_slice(&at.as_nanos().to_be_bytes());
    Dedup {
        memo: keccak256(&preimage).0,
        created_at_nanos: at.as_nanos(),
    }
}

/// Refuses, before the pull, an amount no give-back could return: at or under the ledger's fee
/// it would come back as nothing, and only cost a human a pull to check.
pub fn worth_pulling(ledger: Principal, amount: TokenAmount) -> Result<(), PoolError> {
    let fee = crate::storage::pool::ledger()
        .settings
        .and_then(|settings| settings.fee_of(ledger));
    if fee.is_some_and(|fee| amount <= fee) {
        return Err(PoolError::NotAllowed(format!(
            "an amount of {amount} is not above the ledger's fee"
        )));
    }
    Ok(())
}

/// Pulls `amount` of `ledger` from `from` into the canister: the ledger's block, or the
/// door's answer with nothing written (a pull to check kept where its outcome is unclear).
pub async fn pull(
    door: &str,
    ledger: Principal,
    from: IcpAccount,
    amount: TokenAmount,
    dedup: Dedup,
    at: Timestamp,
) -> Result<u64, PoolError> {
    match ic::icrc2_pull_fresh(
        ledger,
        Account::of(from.owner, from.subaccount),
        Nat::from(amount),
        dedup,
    )
    .await
    {
        Ok(block) => Ok(block),
        Err(FreshPull::Refused(why)) => Err(PoolError::Ledger(why)),
        Err(FreshPull::NotNow(why)) => Err(PoolError::Unavailable(format!(
            "{why}: nothing moved, ask again"
        ))),
        Err(FreshPull::Unclear(why)) => {
            to_check(door, "pull", ledger, from, amount, dedup.memo, at, &why);
            Err(PoolError::Unavailable(format!(
                "the pull's outcome is unclear ({why}): it is checked by hand and given back if it moved"
            )))
        }
    }
}

/// Gives back money a pull brought in that no line could take (`why`): all of it less the
/// ledger's fee, to the account it came from. One that does not go is kept to check.
pub async fn give_back(
    door: &str,
    ledger: Principal,
    from: IcpAccount,
    amount: TokenAmount,
    fee: Option<TokenAmount>,
    pulled: Dedup,
    why: &str,
) {
    crate::pool::log(&format!(
        "ALERT: {door}: a pull came in that no line takes ({why}); it is given back"
    ));
    let at = Timestamp::from_nanos(ic_cdk::api::time());
    let Some(back) = fee
        .and_then(|fee| amount.checked_sub(fee))
        .filter(|back| *back > TokenAmount::ZERO)
    else {
        to_check(
            door,
            "give-back",
            ledger,
            from,
            amount,
            pulled.memo,
            at,
            "the ledger's fee is not known",
        );
        return;
    };
    let mut preimage = pulled.memo.to_vec();
    preimage.extend_from_slice(b".back");
    let dedup = Dedup {
        memo: keccak256(&preimage).0,
        created_at_nanos: at.as_nanos(),
    };
    if let Err(error) = ic::icrc1_transfer(
        ledger,
        None,
        Account::of(from.owner, from.subaccount),
        Nat::from(back),
        dedup,
    )
    .await
    {
        to_check(
            door,
            "give-back",
            ledger,
            from,
            amount,
            pulled.memo,
            at,
            &error.to_string(),
        );
    }
}

/// Keeps a pull nobody can tell the outcome of, for the watcher to alert and a human to check.
#[allow(clippy::too_many_arguments)]
fn to_check(
    door: &str,
    what: &str,
    ledger: Principal,
    from: IcpAccount,
    amount: TokenAmount,
    memo: [u8; 32],
    at: Timestamp,
    why: &str,
) {
    crate::pool::log(&format!(
        "ALERT: {door}: the {what} of {amount} from {} is unclear ({why}); memo {}",
        from.owner,
        hex::encode(memo)
    ));
    let pull = PullToCheck {
        at,
        ledger: PrincipalBytes(ledger),
        from,
        amount,
        memo,
        what: format!("{door} {what}"),
        why: why.chars().take(256).collect(),
    };
    let kept = timer_state::update(|state| {
        let pulls = state.pulls_to_check.get_or_insert_with(Vec::new);
        pulls.push(pull);
        if pulls.len() > MAX_PULLS_TO_CHECK {
            pulls.remove(0);
        }
        (Ok::<(), String>(()), true)
    });
    if let Err(error) = kept {
        crate::pool::log(&format!("ALERT: a pull to check was not kept: {error}"));
    }
}

/// The pulls to check, oldest first.
pub fn pulls_to_check() -> Vec<PullToCheck> {
    timer_state::get().pulls_to_check.unwrap_or_default()
}
