//! `claim_across_refund` (BSC design 6.2.9, C-10): a controller's door that claims what
//! Across's SpokePool holds back for the canister (a deferred relayer refund leaf: the
//! canister's own reclaims of unfilled ICP payouts), on the pool chain, the only chain the
//! canister deposits from. It reads `getRelayerRefund(USDC, canister)` first and signs only
//! above zero; the transfer it brings is credited by the arrival rule every Across refund
//! meets (D9), its `ClaimedRelayerRefund` naming the canister as caller.

use crate::guards::{require_controller, require_not_halted};
use crate::tx;
use types::config::GasAction;
use types::events::TxPurpose;
use types::swapic::across;
use types::{ChainId, GasAmount, TokenAmount, Wei};

/// `claim_across_refund(chain_id)`'s body: the signed transaction's id in `signed_txs`.
pub async fn claim(chain_id: u64) -> Result<u64, String> {
    require_controller().map_err(|error| format!("{error:?}"))?;
    require_not_halted().map_err(|error| format!("{error:?}"))?;
    let chain = ChainId::new(chain_id);
    let config = crate::storage::config::get();
    if config.pool_chain != Some(chain) {
        return Err("Across refunds are claimed on the pool chain alone".to_string());
    }
    let evm = config
        .evm_chains
        .get(chain)
        .ok_or("the canister signs nothing on the pool chain")?;
    let pins = evm.across.ok_or("the pool chain holds no Across pins")?;
    let gas_limit = evm
        .gas_limit
        .get(GasAction::AcrossClaim)
        .ok_or("the pool chain has no across_claim gas row")?;
    let purpose = TxPurpose::AcrossClaim(chain);
    if tx::in_flight(purpose) {
        return Err("a claim on the pool chain is on its way".to_string());
    }
    let canister = crate::ecdsa::canister_address()
        .await
        .map_err(|error| error.to_string())?;
    let held = crate::evm_reads::eth_call(
        chain,
        canister,
        pins.spoke_pool,
        &across::get_relayer_refund_calldata(evm.usdc, canister),
    )
    .await
    // the kind only: an outcall's text can carry the provider's url and key
    .map_err(|error| format!("the SpokePool's read: {:?}", error.kind()))?;
    let held = held
        .word()
        .map(TokenAmount::from_be_bytes)
        .ok_or("the SpokePool's read answered no amount")?;
    if held == TokenAmount::ZERO {
        return Err("the SpokePool holds nothing back for the canister".to_string());
    }
    crate::gas::ensure_fresh(chain)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))?;
    require_not_halted().map_err(|error| format!("{error:?}"))?;
    let signed = tx::create_and_send(
        purpose,
        chain,
        pins.spoke_pool,
        Wei::ZERO,
        across::claim_relayer_refund_calldata(evm.usdc, canister),
        GasAmount::from(gas_limit),
        GasAction::AcrossClaim,
    )
    .await
    .map_err(|error| error.to_string())?;
    crate::pool::log(&format!(
        "Across: the canister claimed its deferred refunds of {held} on chain {chain}"
    ));
    Ok(signed.id.get())
}
