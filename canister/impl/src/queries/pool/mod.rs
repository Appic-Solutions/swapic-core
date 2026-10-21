//! What the pool's fold holds (interface section 3).

use crate::pool;
use crate::state::Store;
use crate::storage::events::read_state;
use candid::Nat;
use ic_cdk::query;
pub use settlement_api::types::pool::{
    IcpRoutesView, OrphanView, PayoutSettings, PayoutView, PayoutWaiting, PoolAccount,
    PoolAccountView, PoolDepositView, PoolSettings, PoolStateView,
};
use types::hash::PoolRef;

/// The pool's knobs; the launch defaults while none are set.
#[query]
pub fn pool_settings() -> PoolSettings {
    let settings = crate::storage::pool::ledger().settings.unwrap_or_default();
    PoolSettings::from(&settings)
}

/// Whether the pool is open and why not, both pockets' lines, the stakers' assets and the
/// shares outstanding, and the caps.
#[query]
pub fn pool_state() -> PoolStateView {
    let closed = pool::pool_closed();
    pool::state_view(&crate::storage::pool::ledger(), closed)
}

/// An account's shares and what they are worth now. An account that is no account (an
/// address that is none, a subaccount that is not 32 bytes) traps with the reason.
#[query]
pub fn pool_account(account: PoolAccount) -> PoolAccountView {
    let account = types::pool::PoolAccount::try_from(account)
        .unwrap_or_else(|error| ic_cdk::trap(&error.to_string()));
    let ledger = crate::storage::pool::ledger();
    let shares = crate::storage::pool::shares_of(&account);
    PoolAccountView {
        shares: Nat::from(shares.get()),
        value_usdc: Nat::from(pool::value_of(&ledger, shares)),
    }
}

/// A deposit by its ref, if one is registered under it.
#[query]
pub fn pool_deposit(r#ref: Vec<u8>) -> Option<PoolDepositView> {
    let r#ref = PoolRef::new(<[u8; 32]>::try_from(r#ref.as_slice()).ok()?);
    read_state(|state| state.store().pool_deposit(&r#ref))
        .map(|deposit| PoolDepositView::of(r#ref, deposit))
}

/// The Base payouts' knobs; the launch defaults while none are set.
#[query]
pub fn payout_settings() -> PayoutSettings {
    let settings = crate::storage::pool::ledger()
        .lines()
        .settings
        .unwrap_or_default();
    PayoutSettings::from(&settings)
}

/// A payout from the Base pocket by its id, if one was opened under it.
#[query]
pub fn payout(id: u64) -> Option<PayoutView> {
    read_state(|state| state.store().pool_payout(id)).map(|payout| PayoutView::from(&payout))
}

/// The ICP routes every ICP path of the pool reads: the ICP/ckUSDC pool, and each listed
/// token's route. Empty while none are set.
#[query]
pub fn icp_routes() -> IcpRoutesView {
    let settings = crate::storage::pool::ledger().settings;
    IcpRoutesView::from(
        settings
            .as_ref()
            .and_then(|settings| settings.routes.as_ref()),
    )
}

/// The payouts waiting for an order, the longest waiting first, at most 50: what the watcher
/// asks the quoter for, each at its exact amount.
#[query]
pub fn payouts_waiting() -> Vec<PayoutWaiting> {
    pool::pay::waiting()
}

/// The orphans held for a controller: USDC at the canister's Base address no deposit could
/// take, not refunded or booked yet (a deposit's transfer found without its terms only once
/// its 24 hours of waiting for them are over).
#[query]
pub fn pool_orphans() -> Vec<OrphanView> {
    pool::orphans::held(pool::now())
}
