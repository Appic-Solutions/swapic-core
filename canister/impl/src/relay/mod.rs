//! The gasless relay: a user's signed `swapFor`, checked, simulated, signed once and
//! broadcast from this canister's address, which is Swapic's operator.
//!
//! Money-first: the role and the halt are checked before any outcall, the quote hash is
//! recomputed from the params exactly as the router does (spec 4b.5), every field the user
//! read is checked against the params, and the authorization is recovered to the payer
//! before the simulation is bought. One relay per quote, ever: the fold refuses a second,
//! and the router's `usedQuote` refuses one on chain (4b.4).

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::evm_reads::{self, Simulated};
use crate::guards::{require_not_halted, require_quoter};
use crate::storage::events::read_state;
use crate::storage::flow_settings::{self, FlowSettings};
use crate::storage::sanctions::is_sanctioned;
use crate::tx;
use alloy_primitives::{FixedBytes, U256};
use settlement_api::types::flow::FlowCallError;
use types::events::TxPurpose;
use types::router::{
    self, GaslessAuth, ReceiveWithAuthorization, SwapParams, ACTION_DELIVER, DOOR_AUTH_3009,
    DOOR_PERMIT2, DOOR_PERMIT_2612, RAIL_CCTP, RAIL_ECO, RAIL_LOCAL,
};
use types::{ChainId, EvmAddress, GasAmount, QuoteHash, Timestamp, Wei};

/// Why a relay was refused before any outcall.
fn invalid(reason: impl Into<String>) -> FlowCallError {
    FlowCallError::Invalid(reason.into())
}

/// Every check on the params and the authorization that needs no chain: the readable
/// fields, the deadlines inside the window, the rail and the recipient's shape, and the
/// door's own fields. Pure, so the table is a unit test.
pub fn check_readable(
    params: &SwapParams,
    auth: &GaslessAuth,
    chain_id: ChainId,
    now_s: u64,
    window_s: u64,
) -> Result<(), String> {
    if params.payer.is_zero() {
        return Err("payer is the zero address".into());
    }
    if params.tokenIn.is_zero() {
        return Err("tokenIn is the zero address".into());
    }
    if params.amountIn.is_zero() {
        return Err("amountIn is zero".into());
    }
    let now = U256::from(now_s);
    if params.deadline <= now {
        return Err("the quote's deadline has passed".into());
    }
    if params.deadline > U256::from(now_s.saturating_add(window_s)) {
        return Err("the quote's deadline is further ahead than a relay accepts".into());
    }
    match params.rail {
        RAIL_CCTP | RAIL_ECO => {}
        RAIL_LOCAL => {
            if params.dstChainId != chain_id.get() {
                return Err("a local swap delivers on the chain it starts on".into());
            }
            if params.dst.action != ACTION_DELIVER {
                return Err("a local swap only delivers".into());
            }
            if router::evm_recipient(params.dst.recipient).is_none_or(|to| to == EvmAddress::ZERO) {
                return Err("a local swap needs a non-zero EVM recipient".into());
            }
        }
        _ => return Err("the rail is not one Swapic has".into()),
    }
    let deliver_on_cctp = params.rail == RAIL_CCTP && params.dst.action == ACTION_DELIVER;
    if !deliver_on_cctp && router::evm_recipient(params.dst.recipient).is_none() {
        return Err("the recipient is not an EVM address".into());
    }
    if params.dst.deadline < now_s {
        return Err("the destination deadline has passed".into());
    }
    match auth.door {
        DOOR_AUTH_3009 => {
            if auth.validAfter >= now {
                return Err("the authorization is not valid yet".into());
            }
            if auth.validBefore <= now {
                return Err("the authorization has expired".into());
            }
            if auth.signature.len() != 65 {
                return Err("the authorization's signature is not 65 bytes".into());
            }
        }
        DOOR_PERMIT_2612 => {
            if auth.acceptance.len() != 65 {
                return Err("the quote acceptance is not a 65-byte signature".into());
            }
        }
        DOOR_PERMIT2 => {
            if auth.deadline <= now {
                return Err("the Permit2 permit has expired".into());
            }
            if auth.signature.len() != 65 {
                return Err("the Permit2 signature is not 65 bytes".into());
            }
        }
        _ => return Err("the door is not one the router has".into()),
    }
    Ok(())
}

/// The digest the door's signature must recover to the payer over. The 3009 door's is
/// under the token's own domain separator, which the caller read off the token.
pub fn door_digest(
    params: &SwapParams,
    auth: &GaslessAuth,
    quote_hash: QuoteHash,
    chain_id: ChainId,
    router_address: EvmAddress,
    token_separator: Option<[u8; 32]>,
) -> Option<([u8; 32], Vec<u8>)> {
    match auth.door {
        DOOR_AUTH_3009 => {
            let message = ReceiveWithAuthorization {
                from: params.payer,
                to: router::addr(router_address),
                value: params.amountIn,
                validAfter: auth.validAfter,
                validBefore: auth.validBefore,
                nonce: FixedBytes(quote_hash.into_bytes()),
            };
            Some((
                router::receive_with_authorization_digest(token_separator?, &message),
                auth.signature.to_vec(),
            ))
        }
        DOOR_PERMIT_2612 => Some((
            router::acceptance_digest(params, quote_hash, chain_id, router_address),
            auth.acceptance.to_vec(),
        )),
        DOOR_PERMIT2 => Some((
            router::permit2_digest(
                chain_id,
                params,
                quote_hash,
                router_address,
                auth.nonce,
                auth.deadline,
            ),
            auth.signature.to_vec(),
        )),
        _ => None,
    }
}

/// The token's EIP-712 domain separator, read off the token.
async fn token_separator(
    chain_id: ChainId,
    mine: EvmAddress,
    token: EvmAddress,
) -> Result<[u8; 32], FlowCallError> {
    match evm_reads::eth_call(chain_id, mine, token, &router::domain_separator_calldata())
        .await
        .map_err(|e| FlowCallError::Unavailable(e.to_string()))?
    {
        Simulated::Returned(bytes) if bytes.len() == 32 => {
            Ok(bytes.try_into().expect("BUG: checked to be 32 bytes"))
        }
        Simulated::Returned(_) => Err(FlowCallError::Refused(
            "the token answers no EIP-712 domain separator".into(),
        )),
        Simulated::Reverted(message) => Err(FlowCallError::Refused(format!(
            "the token has no DOMAIN_SEPARATOR: {message}"
        ))),
    }
}

/// The gas a relay is given: the estimate with a margin, held to the configured ceiling.
pub fn gas_limit_for(estimate: Option<u64>, settings: &FlowSettings) -> u64 {
    let ceiling = settings.max_gas_limit;
    match estimate {
        Some(estimate) => estimate
            .saturating_mul(13)
            .saturating_div(10)
            .saturating_add(50_000)
            .min(ceiling),
        None => ceiling,
    }
}

/// `relay_gasless`: answers the quote hash the relay is recorded under. Its status is
/// `relay_status(quote_hash)`.
pub async fn relay_gasless(
    chain_id: ChainId,
    params_bytes: &[u8],
    auth_bytes: &[u8],
) -> Result<QuoteHash, FlowCallError> {
    // the role and the halt before anything else, and before any outcall
    require_quoter().map_err(FlowCallError::Guard)?;
    require_not_halted().map_err(FlowCallError::Guard)?;
    let settings = flow_settings::get();
    let router_address = settings
        .router(chain_id)
        .ok_or_else(|| invalid(format!("no router is configured on chain {chain_id}")))?;
    let params = router::decode_params(params_bytes)
        .ok_or_else(|| invalid("params are not an ABI-encoded SwapParams tuple"))?;
    let auth = router::decode_auth(auth_bytes)
        .ok_or_else(|| invalid("auth is not an ABI-encoded GaslessAuth tuple"))?;
    let quote_hash = router::quote_hash(&params, chain_id, router_address);
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    check_readable(
        &params,
        &auth,
        chain_id,
        now_s,
        settings.relay_window.as_secs(),
    )
    .map_err(invalid)?;
    let payer = router::evm(params.payer);
    if is_sanctioned(&types::Address::from(payer)) {
        return Err(FlowCallError::Refused("the payer is sanctioned".into()));
    }
    if let Some(recipient) = router::evm_recipient(params.dst.recipient) {
        if is_sanctioned(&types::Address::from(recipient)) {
            return Err(FlowCallError::Refused("the recipient is sanctioned".into()));
        }
    }
    if read_state(|state| state.flow(&quote_hash).is_some()) {
        return Err(FlowCallError::Refused(format!(
            "quote {quote_hash} was already relayed"
        )));
    }
    let mine = ecdsa::canister_address()
        .await
        .map_err(|e| FlowCallError::Unavailable(e.to_string()))?;
    // the authorization recovers to the payer, so the user signed THESE params
    let separator = if auth.door == DOOR_AUTH_3009 {
        Some(token_separator(chain_id, mine, router::evm(params.tokenIn)).await?)
    } else {
        None
    };
    let (digest, signature) = door_digest(
        &params,
        &auth,
        quote_hash,
        chain_id,
        router_address,
        separator,
    )
    .ok_or_else(|| invalid("the door has no digest"))?;
    if ecdsa::recover_signer(digest, &signature) != Some(payer) {
        return Err(FlowCallError::Refused(
            "the authorization was not signed by the payer over these params".into(),
        ));
    }
    // the halt can land while the reads were out
    require_not_halted().map_err(FlowCallError::Guard)?;
    let data = router::swap_for_calldata(&params, &auth);
    let (simulated, estimate) =
        evm_reads::simulate_and_estimate(chain_id, mine, router_address, &data)
            .await
            .map_err(|e| FlowCallError::Unavailable(e.to_string()))?;
    if let Simulated::Reverted(message) = simulated {
        return Err(FlowCallError::Refused(format!(
            "swapFor reverts in simulation: {message}"
        )));
    }
    let gas_limit = gas_limit_for(estimate, &settings);
    tx::create_and_send(
        TxPurpose::Relay(quote_hash),
        chain_id,
        router_address,
        Wei::ZERO,
        data,
        GasAmount::from(gas_limit),
    )
    .await
    .map_err(|e| FlowCallError::Refused(e.to_string()))?;
    Ok(quote_hash)
}
