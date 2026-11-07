//! `set_pool_settings`: the controller's knobs, checked, then the ledger fees read for
//! ckUSDC and every listed token, then recorded.

#[cfg(test)]
mod tests;

use crate::ic;
use futures::future::join_all;
use settlement_api::types::pool::{
    PayoutSettings as WirePayoutSettings, PoolError, PoolSettings as WireSettings,
};
use types::pool::{LedgerFee, PoolSettings};
use types::TokenAmount;

/// The knobs as the domain takes them, or the refusal: the halt, then the controller, then
/// settings that hold together.
pub fn checked(
    halted: bool,
    is_controller: bool,
    settings: WireSettings,
) -> Result<PoolSettings, PoolError> {
    crate::pool::controller_only(halted, is_controller, "sets the pool's settings")?;
    let settings = PoolSettings::try_from(settings)
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    settings
        .validate()
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(settings)
}

/// The Base payouts' knobs as the domain takes them, or the refusal: the halt, then the
/// controller, then settings that hold together.
pub fn payout_checked(
    halted: bool,
    is_controller: bool,
    settings: WirePayoutSettings,
) -> Result<types::pool::PayoutSettings, PoolError> {
    crate::pool::controller_only(halted, is_controller, "sets the payout settings")?;
    let settings = types::pool::PayoutSettings::try_from(settings)
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    settings
        .validate()
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(settings)
}

/// `settings` with every ledger's fee read: ckUSDC's and each listed token's.
pub async fn with_fees(mut settings: PoolSettings) -> Result<PoolSettings, PoolError> {
    let mut fees = Vec::new();
    for ledger in settings.ledgers() {
        let fee = ic::icrc1_fee(ledger)
            .await
            .map_err(|error| PoolError::Ledger(format!("{ledger}'s icrc1_fee: {error}")))?;
        let fee = TokenAmount::try_from(fee)
            .ok()
            .filter(|fee| fee.try_into_u128().is_some())
            .ok_or_else(|| PoolError::Ledger(format!("{ledger}'s fee is too large")))?;
        fees.push(LedgerFee { ledger, fee });
    }
    settings.ledger_fees = fees;
    Ok(settings)
}

/// `set_icp_routes`' arguments as the domain takes them, nothing read yet, or the refusal:
/// the halt, then the controller.
pub fn routes_checked(
    halted: bool,
    is_controller: bool,
    icp_ckusdc_pool: Option<candid::Principal>,
    routes: Vec<settlement_api::types::pool::IcpRoute>,
) -> Result<types::pool::IcpRoutes, PoolError> {
    crate::pool::controller_only(halted, is_controller, "sets the ICP routes")?;
    Ok(types::pool::IcpRoutes {
        icp_ckusdc_pool: icp_ckusdc_pool.map(types::pool::PrincipalBytes),
        icp_ledger: None,
        routes: routes.into_iter().map(Into::into).collect(),
        ledger_fees: Vec::new(),
        pool_tokens: Vec::new(),
    })
}

/// The ICP ledger an ICP/ckUSDC pool trades: its token that is not `ckusdc`, None for a pool
/// that does not trade ckUSDC against one other ledger.
pub fn icp_ledger_of(
    token0: &str,
    token1: &str,
    ckusdc: candid::Principal,
) -> Option<candid::Principal> {
    let (token0, token1) = (
        candid::Principal::from_text(token0).ok()?,
        candid::Principal::from_text(token1).ok()?,
    );
    match (token0 == ckusdc, token1 == ckusdc) {
        (true, false) => Some(token1),
        (false, true) => Some(token0),
        _ => None,
    }
}

/// `routes` with the ICP ledger read off the ICP/ckUSDC pool's metadata and every route
/// ledger's fee read (`icrc1_fee`), then checked against the pool's ckUSDC ledger. Each pool
/// and each ledger is asked once, and all of them at once.
/// The most calls a route set's reads keep open at once. On prod, 22 reads at once failed
/// with "Couldn't send message" on whichever call went 22nd, and 21 passed.
pub const READS_AT_ONCE: usize = 8;

/// `read` of every item, at most [`READS_AT_ONCE`] open at a time, the answers in order.
async fn in_batches<T, A, F>(items: &[T], read: impl Fn(&T) -> F) -> Vec<A>
where
    F: std::future::Future<Output = A>,
{
    let mut answers = Vec::with_capacity(items.len());
    for batch in items.chunks(READS_AT_ONCE) {
        answers.extend(join_all(batch.iter().map(&read)).await);
    }
    answers
}

pub async fn routes_with_reads(
    mut routes: types::pool::IcpRoutes,
    ckusdc: candid::Principal,
) -> Result<types::pool::IcpRoutes, PoolError> {
    // every route pool's tokens, the ICP/ckUSDC pool's among them, read once here and never
    // per swap
    let pools = routes.pools();
    let answers = in_batches(&pools, |pool| ic::pool_metadata(*pool)).await;
    let mut pool_tokens = Vec::new();
    for (pool, answer) in pools.into_iter().zip(answers) {
        let metadata =
            answer.map_err(|error| PoolError::Ledger(format!("{pool}'s metadata: {error}")))?;
        if routes.icp_pool() == Some(pool) {
            let icp = icp_ledger_of(&metadata.token0.address, &metadata.token1.address, ckusdc)
                .ok_or_else(|| {
                    PoolError::BadSettings(format!(
                        "{pool} is no pool of ckUSDC and one other token"
                    ))
                })?;
            routes.icp_ledger = Some(types::pool::PrincipalBytes(icp));
        }
        let token = |text: &str| {
            candid::Principal::from_text(text).map_err(|_| {
                PoolError::BadSettings(format!("{pool} names a token that is no principal"))
            })
        };
        pool_tokens.push(types::pool::PoolTokens {
            pool,
            token0: token(&metadata.token0.address)?,
            token1: token(&metadata.token1.address)?,
        });
    }
    routes.pool_tokens = pool_tokens;
    let ledgers: Vec<_> = routes
        .ledgers()
        .into_iter()
        .filter(|ledger| *ledger != ckusdc)
        .collect();
    let answers = in_batches(&ledgers, |ledger| ic::icrc1_fee(*ledger)).await;
    let mut fees = Vec::new();
    for (ledger, answer) in ledgers.into_iter().zip(answers) {
        let fee =
            answer.map_err(|error| PoolError::Ledger(format!("{ledger}'s icrc1_fee: {error}")))?;
        let fee = TokenAmount::try_from(fee)
            .ok()
            .filter(|fee| fee.try_into_u128().is_some())
            .ok_or_else(|| PoolError::Ledger(format!("{ledger}'s fee is too large")))?;
        fees.push(LedgerFee { ledger, fee });
    }
    routes.ledger_fees = fees;
    routes
        .validate(ckusdc)
        .map_err(|error| PoolError::BadSettings(error.to_string()))?;
    Ok(routes)
}
