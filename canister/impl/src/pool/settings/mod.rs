//! `set_pool_settings`: the controller's knobs, checked, then the ledger fees read for
//! ckUSDC and every listed token, then recorded.

#[cfg(test)]
mod tests;

use crate::ic;
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
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller sets the pool's settings".to_string(),
        ));
    }
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
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller sets the payout settings".to_string(),
        ));
    }
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
    if halted {
        return Err(PoolError::Halted);
    }
    if !is_controller {
        return Err(PoolError::NotAllowed(
            "only a controller sets the ICP routes".to_string(),
        ));
    }
    Ok(types::pool::IcpRoutes {
        icp_ckusdc_pool: icp_ckusdc_pool.map(types::pool::PrincipalBytes),
        icp_ledger: None,
        routes: routes.into_iter().map(Into::into).collect(),
        ledger_fees: Vec::new(),
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
/// ledger's fee read (`icrc1_fee`), then checked against the pool's ckUSDC ledger.
pub async fn routes_with_reads(
    mut routes: types::pool::IcpRoutes,
    ckusdc: candid::Principal,
) -> Result<types::pool::IcpRoutes, PoolError> {
    if let Some(pool) = routes.icp_pool() {
        let metadata = ic::pool_metadata(pool)
            .await
            .map_err(|error| PoolError::Ledger(format!("{pool}'s metadata: {error}")))?;
        let icp = icp_ledger_of(&metadata.token0.address, &metadata.token1.address, ckusdc)
            .ok_or_else(|| {
                PoolError::BadSettings(format!("{pool} is no pool of ckUSDC and one other token"))
            })?;
        routes.icp_ledger = Some(types::pool::PrincipalBytes(icp));
    }
    let mut fees = Vec::new();
    for ledger in routes.ledgers() {
        if ledger == ckusdc {
            continue;
        }
        let fee = ic::icrc1_fee(ledger)
            .await
            .map_err(|error| PoolError::Ledger(format!("{ledger}'s icrc1_fee: {error}")))?;
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
