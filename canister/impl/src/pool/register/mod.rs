//! `register_pool_deposit`, at the user's commit (interface section 4): the quoter's terms
//! checked against the ref they commit to, the clock, the sanctions set and the pool's
//! limits, then the hold taken, all in one message. And the expiry of a registration whose
//! money never came: its hold goes back.

#[cfg(test)]
mod tests;

use crate::state::{State, Store};
use crate::storage::events::read_state;
use crate::storage::pool::record_at;
use settlement_api::types::pool::PoolError;
use std::time::Duration;
use types::hash::PoolRef;
use types::pool::{check_ref, check_terms, DepositKind, PoolAccount, PoolStep, Registration};
use types::{Address, Timestamp};

/// The furthest ahead a registration may expire: the order's deadline plus the rail's
/// arrival time.
pub const MAX_EXPIRY: Duration = Duration::from_secs(15 * 60);

/// Every rule a registration meets, in the interface's order after the guards: settings,
/// open for swaps, the ref, unknown, the expiry, the token, the sanctions, the open
/// limits, then the swap's or the stake's limits.
pub fn decide<S: Store>(
    state: &State<S>,
    r#ref: PoolRef,
    terms: &Registration,
    now: Timestamp,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<(), PoolError> {
    let ledger = state.pool();
    check_ref(&ledger, r#ref, terms, &state.pool_view())?;
    if terms.expires_at <= now {
        return Err(PoolError::Expired);
    }
    let latest = now
        .checked_add(MAX_EXPIRY)
        .unwrap_or(Timestamp::from_nanos(u64::MAX));
    if terms.expires_at > latest {
        return Err(PoolError::NotAllowed(format!(
            "expires_at is more than {} s away",
            MAX_EXPIRY.as_secs()
        )));
    }
    if let DepositKind::Swap { token_out, .. } = terms.kind {
        let settings = ledger
            .settings
            .as_ref()
            .ok_or_else(|| PoolError::Closed("the pool has no settings".to_string()))?;
        if !settings.is_listed(token_out) {
            return Err(PoolError::TokenNotListed(token_out));
        }
    }
    screen(terms, sanctioned)?;
    check_terms(&ledger, terms)?;
    Ok(())
}

/// The payer, and the owner whoever it is: an EVM address as the set keys it, a principal
/// as its text.
fn screen(terms: &Registration, sanctioned: impl Fn(&Address) -> bool) -> Result<(), PoolError> {
    if sanctioned(&Address::from(terms.src_payer)) {
        return Err(PoolError::Sanctioned("the payer is sanctioned".to_string()));
    }
    let owner = match terms.kind.owner() {
        PoolAccount::Evm(address) => Some(Address::from(address)),
        PoolAccount::Icp(account) => account.owner.to_text().parse::<Address>().ok(),
    };
    if owner.is_some_and(|owner| sanctioned(&owner)) {
        return Err(PoolError::Sanctioned("the owner is sanctioned".to_string()));
    }
    Ok(())
}

/// Checks and appends in one message, with no await between, so the hold is taken against
/// the pockets the checks read.
pub fn register(
    r#ref: PoolRef,
    terms: Registration,
    now: Timestamp,
    sanctioned: impl Fn(&Address) -> bool,
) -> Result<(), PoolError> {
    read_state(|state| decide(state, r#ref, &terms, now, sanctioned))?;
    record_at(
        PoolStep::Registered {
            r#ref,
            deposit: terms,
        },
        now,
    )
    .map(drop)
    .map_err(|error| PoolError::NotAllowed(error.to_string()))
}

/// Whether `terms` are the registration already stored under `r#ref` (P13a asks after a
/// commit that met `Known`): the same kind, fees, minimum, payer, rail and expiry, and the
/// salt the ref commits. Reads the fold only, and writes nothing.
pub fn agreed<S: Store>(
    state: &State<S>,
    r#ref: PoolRef,
    terms: &Registration,
) -> Result<bool, PoolError> {
    let stored = state
        .store()
        .pool_deposit(&r#ref)
        .ok_or(PoolError::Unknown)?
        .terms;
    Ok(stored.kind == terms.kind
        && stored.premium == terms.premium
        && stored.rebalance_share == terms.rebalance_share
        && stored.min_usdc == terms.min_usdc
        && stored.src_payer == terms.src_payer
        && stored.rail == terms.rail
        && stored.expires_at == terms.expires_at
        && stored.salt == terms.salt)
}

/// What one pass of the sweep did to the pool's registrations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Expiry {
    pub expired: usize,
    pub skipped: usize,
    /// Whether a further one sat behind the cap.
    pub more: bool,
}

/// The registrations past their expiry and grace with nothing arrived, at most `cap` of them,
/// read off the open index.
pub fn due<S: Store>(store: &S, now: Timestamp, cap: usize) -> (Vec<PoolRef>, bool) {
    let ledger = store.pool_ledger();
    let grace = ledger
        .settings
        .as_ref()
        .map(|settings| settings.deposit_grace)
        .unwrap_or_default();
    let mut due: Vec<PoolRef> = ledger
        .open
        .iter()
        .filter(|r#ref| {
            store
                .pool_deposit(r#ref)
                .is_some_and(|deposit| deposit.terms.credit_until(grace) < now)
        })
        .take(cap.saturating_add(1))
        .copied()
        .collect();
    let more = due.len() > cap;
    due.truncate(cap);
    (due, more)
}

/// Expires what is due: pre-money, so cheap, and bounded per pass.
pub fn expire_due(now: Timestamp, cap: usize) -> Expiry {
    let (due, more) = read_state(|state| due(state.store(), now, cap));
    let mut expiry = Expiry {
        more,
        ..Expiry::default()
    };
    for r#ref in due {
        match record_at(PoolStep::Expired { r#ref }, now) {
            Ok(_) => expiry.expired += 1,
            Err(_) => expiry.skipped += 1,
        }
    }
    expiry
}
