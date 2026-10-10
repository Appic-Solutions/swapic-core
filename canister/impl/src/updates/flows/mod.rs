//! The flow doors: the gasless relay, the two ICP legs, and their settings.

use crate::guards::require_controller;
use crate::relay;
use crate::storage::flow_settings;
use ic_cdk::update;
pub use settlement_api::types::events::Hash32;
pub use settlement_api::types::flow::{FlowCallError, FlowSettings};
use types::ChainId;

/// Controller-only. The routers, the ck canisters, the ICPSwap pools a quote may name,
/// and the windows the flows read.
#[update]
pub fn set_flow_settings(settings: FlowSettings) -> Result<(), FlowCallError> {
    require_controller().map_err(FlowCallError::Guard)?;
    flow_settings::set(settings).map_err(FlowCallError::Invalid)
}

/// Quoter-only, refused while halted. Relays a user's signed `swapFor` through the router
/// on `chain_id`: `params` and `auth` are the ABI encodings of the `SwapParams` and
/// `GaslessAuth` tuples. The quote hash is recomputed from the params as the router does,
/// every readable field is checked against them, the authorization must recover to the
/// payer, and `swapFor` must succeed in simulation before the canister signs it once and
/// records `GaslessRelayed`. Answers the quote hash; `relay_status` follows it.
#[update]
pub async fn relay_gasless(
    chain_id: u64,
    params: Vec<u8>,
    auth: Vec<u8>,
) -> Result<Hash32, FlowCallError> {
    relay::relay_gasless(ChainId::new(chain_id), &params, &auth)
        .await
        .map(|quote_hash| quote_hash.into_bytes())
}

use crate::engine::flows::{self as flow_engine, Busy, FlowDriveError};
use crate::guards::{require_not_halted, require_quoter};
use crate::storage::events::read_state;
use crate::storage::sanctions::is_sanctioned;
pub use candid::Nat;
pub use settlement_api::types::flow::IcpToEvmQuote;
use types::flow::Stage;
use types::{FlowKind, FlowStatus, FlowStep, QuoteHash, Timestamp, TokenAmount};

/// The longest a registered quote may wait for its user.
const MAX_QUOTE_LIFE_S: u64 = 86_400;

fn invalid(reason: impl Into<String>) -> FlowCallError {
    FlowCallError::Invalid(reason.into())
}

fn token_amount(field: &str, value: &Nat) -> Result<TokenAmount, FlowCallError> {
    TokenAmount::from_canonical_nat(value.clone())
        .filter(|amount| *amount > TokenAmount::ZERO)
        .ok_or_else(|| invalid(format!("{field} must be above zero and fit 128 bits")))
}

fn now_s() -> u64 {
    Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get()
}

fn check_expiry(expires_at_s: u64) -> Result<(), FlowCallError> {
    let now = now_s();
    if expires_at_s <= now || expires_at_s > now + MAX_QUOTE_LIFE_S {
        return Err(invalid("expires_at_s must be in the next day"));
    }
    Ok(())
}

/// Quoter-only, refused while halted. Registers an ICP to EVM quote durably, with the
/// user's original destination terms (its route), BEFORE the user is told what to
/// approve (spec 4b.1). Answers the quote hash: the user approves the canister with that
/// hash as the spender subaccount, and only that allowance is ever pulled.
#[update]
pub fn register_icp_quote(quote: IcpToEvmQuote) -> Result<Hash32, FlowCallError> {
    require_not_halted().map_err(FlowCallError::Guard)?;
    require_quoter().map_err(FlowCallError::Guard)?;
    let settings = flow_settings::get();
    let ckusdc = settings
        .ckusdc_ledger
        .ok_or_else(|| invalid("no ckUSDC ledger is configured"))?;
    if settings.ck_minter.is_none() || settings.ethereum_usdc.is_none() {
        return Err(invalid(
            "the ck minter and Ethereum USDC must be configured",
        ));
    }
    if settings.router(settings.ethereum).is_none() {
        return Err(invalid("no router is configured on Ethereum"));
    }
    let amount_in = token_amount("amount_in", &quote.amount_in)?;
    let min_ckusdc = token_amount("min_ckusdc", &quote.min_ckusdc)?;
    match quote.pool {
        None if quote.src_ledger != ckusdc => {
            return Err(invalid("a token other than ckUSDC needs an ICPSwap pool"))
        }
        Some(_) if quote.src_ledger == ckusdc => {
            return Err(invalid("ckUSDC is withdrawn as it is, with no pool"))
        }
        Some(pool) if !settings.icpswap_pools.contains(&pool) => {
            return Err(invalid("the pool is not one the canister swaps on"))
        }
        _ => {}
    }
    let route = types::router::decode_params(&quote.route)
        .ok_or_else(|| invalid("route is not an ABI-encoded SwapParams tuple"))?;
    let recipient = types::router::evm_recipient(route.dst.recipient)
        .ok_or_else(|| invalid("the destination recipient must be an EVM address"))?;
    if is_sanctioned(&types::Address::from(recipient)) {
        return Err(FlowCallError::Refused("the recipient is sanctioned".into()));
    }
    check_expiry(quote.expires_at_s)?;
    let domain = types::flow::IcpToEvmQuote {
        user: quote.user.as_slice().to_vec(),
        src_ledger: quote.src_ledger.as_slice().to_vec(),
        amount_in,
        pool: quote.pool.map(|pool| pool.as_slice().to_vec().into()),
        min_ckusdc,
        route: quote.route,
        expires_at: types::UnixSeconds::new(quote.expires_at_s),
        nonce: quote.nonce,
    };
    let quote_hash = domain.hash();
    flow_engine::record(quote_hash, FlowStep::IcpQuoteRegistered { quote: domain })
        .map_err(|e| FlowCallError::Refused(e.to_string()))?;
    Ok(quote_hash.into_bytes())
}

/// Quoter-only, refused while halted. Starts a registered ICP to EVM quote: pulls the
/// user's token with `icrc2_transfer_from` from the allowance whose spender subaccount is
/// the quote hash (and from no other), and answers what was pulled. The engine then swaps
/// on ICPSwap when needed, withdraws through the ck minter to the canister's own Ethereum
/// address, and continues on Ethereum once the USDC lands.
#[update]
pub async fn start_icp_swap(quote_hash: Hash32) -> Result<Nat, FlowCallError> {
    require_not_halted().map_err(FlowCallError::Guard)?;
    require_quoter().map_err(FlowCallError::Guard)?;
    let quote_hash = QuoteHash::new(quote_hash);
    let flow = read_state(|state| state.flow(&quote_hash))
        .ok_or_else(|| invalid("no quote is registered under that hash"))?;
    if flow.kind != FlowKind::IcpToEvm || flow.status != FlowStatus::Registered {
        return Err(FlowCallError::Refused(format!(
            "the quote is {:?}, not a registered ICP to EVM quote",
            flow.status
        )));
    }
    let quote = flow
        .icp_quote
        .clone()
        .expect("BUG: an ICP to EVM flow holds its quote");
    if now_s() > quote.expires_at.get() {
        return Err(FlowCallError::Refused("the quote has expired".into()));
    }
    let _busy = Busy::take(quote_hash)
        .ok_or_else(|| FlowCallError::Refused("the quote is being started already".into()))?;
    flow_engine::record(quote_hash, FlowStep::Begun { stage: Stage::Pull })
        .map_err(|e| FlowCallError::Refused(e.to_string()))?;
    let flow = read_state(|state| state.flow(&quote_hash)).expect("BUG: just recorded");
    match flow_engine::pull(quote_hash, &flow, &quote).await {
        Ok(amount) => Ok(Nat::from(amount)),
        Err(FlowDriveError::Call(crate::ic::CallError::Refused(reason))) => Err(
            FlowCallError::Refused(format!("the ledger refused the pull: {reason}")),
        ),
        Err(error) => Err(FlowCallError::Unavailable(format!("{error:?}"))),
    }
}

pub use settlement_api::types::flow::{EvmQuoteRegistered, EvmToIcpQuote};

/// Quoter-only, refused while halted. Registers an EVM to ICP quote durably BEFORE the
/// router quote is built (spec 4b.8): the user's principal is recorded with it, and the
/// answer names where the ckUSDC must be deposited, the canister's subaccount named by the
/// router params' `ref` (`reference`), so the route's helper deposit carries it. The
/// engine finds the ckUSDC there, swaps it on ICPSwap and pays the user. A quote for
/// ckUSDC itself names no pool, and its ckUSDC is forwarded as it is.
#[update]
pub fn register_evm_quote(quote: EvmToIcpQuote) -> Result<EvmQuoteRegistered, FlowCallError> {
    require_not_halted().map_err(FlowCallError::Guard)?;
    require_quoter().map_err(FlowCallError::Guard)?;
    let settings = flow_settings::get();
    let ckusdc = settings
        .ckusdc_ledger
        .ok_or_else(|| invalid("no ckUSDC ledger is configured"))?;
    let min_out = token_amount("min_out", &quote.min_out)?;
    match quote.pool {
        None if quote.target_ledger != ckusdc => {
            return Err(invalid("a token other than ckUSDC needs an ICPSwap pool"))
        }
        Some(pool) if !settings.icpswap_pools.contains(&pool) => {
            return Err(invalid("the pool is not one the canister swaps on"))
        }
        _ => {}
    }
    check_expiry(quote.expires_at_s)?;
    let domain = types::flow::EvmToIcpQuote {
        user: quote.user.as_slice().to_vec(),
        target_ledger: quote.target_ledger.as_slice().to_vec(),
        pool: quote.pool.map(|pool| pool.as_slice().to_vec().into()),
        min_out,
        src_chain: types::ChainId::new(quote.src_chain),
        expires_at: types::UnixSeconds::new(quote.expires_at_s),
        nonce: quote.nonce,
        reference: QuoteHash::new(quote.reference),
    };
    let quote_hash = domain.hash();
    flow_engine::record(quote_hash, FlowStep::EvmQuoteRegistered { quote: domain })
        .map_err(|e| FlowCallError::Refused(e.to_string()))?;
    Ok(EvmQuoteRegistered {
        quote_hash: quote_hash.into_bytes(),
        owner: ic_cdk::api::id(),
        subaccount: quote_hash.into_bytes(),
    })
}
