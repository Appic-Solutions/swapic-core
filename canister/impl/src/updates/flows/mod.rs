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

/// Watcher-only (the submit API runs as the watcher), refused while halted. Relays a
/// user's gasless order through Swapic on `chain_id` as `runFor(order, auth)`: `order` is
/// `abi.encode(Order)` of ABI v3 and `auth` is `abi.encode(GaslessAuth)`, each one dynamic
/// tuple with its leading offset word, as viem's `encodeAbiParameters` writes it. The
/// order id is recomputed from the order and the chain's Swapic (the 3009 door's nonce
/// must be it); the order must pay this canister exactly one fee step of at least the
/// chain's `min_network_fee` for its rail, plus on CCTP the destination's for the arrival;
/// it is signed with exactly `gas_limit` (the limit the quoter charged for), or the
/// chain's maximum for the action when that is lower, and held to the gas rule. Signed
/// once per order id: a second call answers the first one's id. Answers the signed
/// transaction's id in `signed_txs`; `relay_status(order_id)` follows it.
#[update]
pub async fn relay_gasless(
    chain_id: u64,
    order: Vec<u8>,
    auth: Vec<u8>,
    gas_limit: u64,
) -> Result<u64, String> {
    relay::relay_gasless(ChainId::new(chain_id), &order, &auth, gas_limit).await
}

use crate::engine::flows::{self as flow_engine, Busy, FlowDriveError};
use crate::guards::{require_not_halted, require_quoter};
use crate::storage::events::read_state;
use crate::storage::sanctions::is_sanctioned;
pub use candid::Nat;
pub use settlement_api::types::flow::{FlowMode, IcpToEvmQuote, Recipe};
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

/// The ICP flows still speak router ABI v2, so they are off until they move to ABI v3
/// with the pool (P11), unless a controller turns them on.
fn require_icp_enabled() -> Result<(), FlowCallError> {
    if !flow_settings::get().icp_enabled {
        return Err(FlowCallError::Refused(
            "the ICP flows are off until they move to ABI v3".into(),
        ));
    }
    Ok(())
}

fn check_mode(mode: FlowMode) -> Result<(), FlowCallError> {
    match mode {
        FlowMode::CkMinter => Ok(()),
        // the pool's guard first, as every pool outflow calls it; the pool itself opens
        // with P11
        FlowMode::Pool => {
            crate::books::require_pool_open()
                .map_err(|closed| FlowCallError::Refused(closed.to_string()))?;
            Err(FlowCallError::Refused("the pool is not open".into()))
        }
    }
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
    require_icp_enabled()?;
    check_mode(quote.mode)?;
    let settings = flow_settings::get();
    let ckusdc = settings
        .ckusdc_ledger
        .ok_or_else(|| invalid("no ckUSDC ledger is configured"))?;
    if settings.ck_minter.is_none() || settings.ethereum_usdc.is_none() {
        return Err(invalid(
            "the ck minter and Ethereum USDC must be configured",
        ));
    }
    if crate::storage::config::get()
        .evm_chains
        .get(settings.ethereum)
        .is_none()
    {
        return Err(invalid("no Swapic is configured on Ethereum"));
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
    require_icp_enabled()?;
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
    require_icp_enabled()?;
    check_mode(quote.mode)?;
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

/// Watcher-only, refused while halted. Finishes a CCTP arrival on `chain_id`, the
/// destination. `message` and `attestation` are Circle's, the message 696 bytes with the
/// v3 envelope as its hook data; `src_tx_hash` is the source transaction that burned it,
/// which the watcher has proven paid the canister's fee (I-3.4); `gas_limit` is the limit
/// the quoter charged for. From the envelope and the clock: a Deliver is received on the
/// message transmitter; a Swap (with `recipe`, required) or a CkDeposit (with none) is
/// completed on the CctpReceiver before its deadline, and released after it. Signed with
/// exactly `gas_limit`, or the chain's maximum for the action when that is lower, and held
/// to the gas rule; once per message and action, and one message per source transaction.
/// Answers the signed transaction's id in `signed_txs`.
#[update]
pub async fn finish_cctp(
    chain_id: u64,
    message: Vec<u8>,
    attestation: Vec<u8>,
    recipe: Option<Recipe>,
    src_tx_hash: Vec<u8>,
    gas_limit: u64,
) -> Result<u64, String> {
    use crate::cctp::{parse_address, FinishError, GivenRecipe};
    let recipe = recipe
        .map(|recipe| {
            Ok::<_, FinishError>(GivenRecipe {
                target: parse_address(&recipe.target)
                    .ok_or(FinishError::RecipeAddress("target"))?,
                data: recipe.data,
                scaler: recipe
                    .scaler
                    .map(|scaler| {
                        parse_address(&scaler).ok_or(FinishError::RecipeAddress("scaler"))
                    })
                    .transpose()?,
            })
        })
        .transpose()
        .map_err(|e| e.to_string())?;
    crate::cctp::finish_cctp(
        ChainId::new(chain_id),
        &message,
        &attestation,
        recipe,
        &src_tx_hash,
        gas_limit,
    )
    .await
}
