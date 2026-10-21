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
