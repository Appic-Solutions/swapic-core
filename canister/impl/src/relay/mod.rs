//! The gasless relay, design C: a user's signed order, checked, signed once as
//! `Swapic.runFor` from this canister's address (Swapic's `canister()`, the only caller
//! `runFor` admits), and handed to the watcher through `signed_txs`.
//!
//! Money-first: the role and the halt come before anything else; the order id is
//! recomputed from the order exactly as every door does; the order must pay this canister
//! exactly one fee step that covers what it signs for the order (the relay, and on CCTP
//! the arrival too); the gas is the limit the quoter charged for, held to the chain's
//! maximum for the action and to the gas rule. One relay per order id, ever: the fold
//! refuses a second, a second call answers the first one's id, and Swapic's `used`
//! refuses a replay on chain.
//!
//! The canister simulates only when `simulate_before_sign` is on: the submit API simulates
//! `runFor` from the canister's address before it calls here.

#[cfg(test)]
mod tests;

use crate::ecdsa;
use crate::engine::flows::Busy;
use crate::evm_reads::{self, Simulated};
use crate::guards::{require_not_halted, require_watcher};
use crate::storage::sanctions::is_sanctioned;
use crate::storage::{config, flow_settings};
use crate::tx;
use alloy_primitives::U256;
use thiserror::Error;
use types::config::{ChainTable, EvmChain, GasAction};
use types::events::TxPurpose;
use types::swapic::{
    self, Action, CctpBurn, Envelope, EnvelopeError, GaslessAuth, Order, OrderId, DOOR_AUTH_3009,
    DOOR_PERMIT2, DOOR_PERMIT_2612, NATIVE, PUBLISH_AND_FUND,
};
use types::{CctpDomain, ChainId, EvmAddress, GasAmount, QuoteHash, Timestamp, TokenAmount, Wei};

/// The least time a relayed order's deadline must still be ahead of the canister's clock:
/// a signature, the watcher's next poll of `signed_txs`, and a block on the slowest chain.
pub const RELAY_MARGIN_S: u64 = 30;

/// The least gas any transaction has: what a plain transfer costs.
pub const MIN_GAS_LIMIT: u64 = 21_000;

/// Why a relay was refused. Nothing was signed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RelayError {
    #[error("chain {0} is not one the canister signs on")]
    UnknownChain(ChainId),
    #[error("order is not abi.encode of an ABI v3 Order")]
    NotAnOrder,
    #[error("auth is not abi.encode of a GaslessAuth")]
    NotAnAuth,
    #[error("door {0} is not one Swapic has")]
    UnknownDoor(u8),
    #[error("the authorization's nonce is not the order id {0}, which Swapic recomputes")]
    WrongOrderId(OrderId),
    #[error("the authorization {0}")]
    Authorization(&'static str),
    #[error("the order {0}")]
    Order(&'static str),
    #[error("the order's deadline {deadline} is not between {earliest} and {latest}")]
    Deadline {
        deadline: U256,
        earliest: u64,
        latest: u64,
    },
    #[error("the order pays the canister no fee step")]
    NoFeeStep,
    #[error("the order pays the canister {0} fee steps, and one fee pays for one order")]
    FeeSteps(usize),
    #[error("the fee step pays {paid}, below the {needed} the order's transactions need")]
    FeeBelowMinimum {
        paid: TokenAmount,
        needed: TokenAmount,
    },
    #[error("the order burns on CCTP {0} times, and one fee pays for one arrival")]
    Burns(usize),
    #[error("the order both burns on CCTP and publishes on Eco")]
    TwoRails,
    #[error("the burn {0}")]
    Burn(&'static str),
    #[error("the burn's destination domain {0} is no chain the canister signs on")]
    UnknownDestination(u32),
    #[error("the burn's envelope: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("the envelope {0}")]
    Arrival(&'static str),
    #[error("a gas limit of {0} is below the {MIN_GAS_LIMIT} any transaction needs")]
    GasLimitTooLow(u64),
    #[error("chain {chain_id} gives a {action:?} no gas")]
    NoGasLimit {
        chain_id: ChainId,
        action: GasAction,
    },
}

/// What the order's checks read besides the order: the chains the canister signs on and
/// their CCTP domains, and the canister's own address, which the fee step pays.
pub struct Chains<'a> {
    pub chain_id: ChainId,
    pub evm_chains: &'a ChainTable<EvmChain>,
    pub domains: &'a ChainTable<CctpDomain>,
    pub canister: EvmAddress,
}

/// What the canister signs for an order, read from the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relay {
    pub order_id: OrderId,
    /// The relay's own action, by the order's rail.
    pub action: GasAction,
    /// What the fee step pays the canister.
    pub fee: TokenAmount,
    /// On CCTP, the arrival the fee pays for too: its chain, its action and its recipient.
    pub arrival: Option<Arrival>,
    /// The limit the relay is signed with.
    pub gas_limit: GasAmount,
}

/// The destination side of a CCTP order, as its burn seals it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub chain_id: ChainId,
    pub action: GasAction,
    pub recipient: EvmAddress,
}

/// The limit a transaction for `action` on `chain` is signed with: exactly the one asked
/// for, or the chain's maximum for the action when that is lower.
pub fn signing_gas(
    chain_id: ChainId,
    chain: &EvmChain,
    action: GasAction,
    requested: u64,
) -> Result<GasAmount, RelayError> {
    if requested < MIN_GAS_LIMIT {
        return Err(RelayError::GasLimitTooLow(requested));
    }
    let gas = GasAmount::from(requested).min(chain.gas_limit.get(action));
    if gas < GasAmount::from(MIN_GAS_LIMIT) {
        return Err(RelayError::NoGasLimit { chain_id, action });
    }
    Ok(gas)
}

/// The door's own fields at `now_s`, and on the 3009 door the nonce, which IS the order
/// id: a client that signed another order, or this one for another Swapic, is refused
/// here and not by a revert on chain.
fn check_auth(auth: &GaslessAuth, id: OrderId, now_s: u64) -> Result<(), RelayError> {
    let now = U256::from(now_s);
    match auth.door {
        DOOR_AUTH_3009 => {
            if auth.nonce != id.as_nonce() {
                return Err(RelayError::WrongOrderId(id));
            }
            if auth.validAfter >= now {
                return Err(RelayError::Authorization("is not valid yet"));
            }
            if auth.validBefore <= now {
                return Err(RelayError::Authorization("has expired"));
            }
            if auth.signature.len() != 65 {
                return Err(RelayError::Authorization("signature is not 65 bytes"));
            }
        }
        DOOR_PERMIT_2612 => {
            if auth.acceptance.is_empty() {
                return Err(RelayError::Authorization("carries no quote acceptance"));
            }
        }
        DOOR_PERMIT2 => {
            if auth.deadline <= now {
                return Err(RelayError::Authorization("Permit2 permit has expired"));
            }
            if auth.signature.is_empty() {
                return Err(RelayError::Authorization("carries no Permit2 signature"));
            }
        }
        door => return Err(RelayError::UnknownDoor(door)),
    }
    Ok(())
}

/// The arrival a CCTP burn seals, checked as the destination contracts will check it
/// (abi.md rule 10), so the canister never relays a burn whose arrival cannot be finished.
fn arrival_of(burn: &CctpBurn, source: &EvmChain, chains: &Chains) -> Result<Arrival, RelayError> {
    if burn.burn_token != source.usdc {
        return Err(RelayError::Burn("is not of the chain's USDC"));
    }
    let chain_id = chains
        .domains
        .0
        .iter()
        .find(|(_, domain)| domain.get() == burn.destination_domain)
        .map(|(chain, _)| *chain)
        .filter(|chain| *chain != chains.chain_id)
        .ok_or(RelayError::UnknownDestination(burn.destination_domain))?;
    let destination = chains
        .evm_chains
        .get(chain_id)
        .ok_or(RelayError::UnknownDestination(burn.destination_domain))?;
    let envelope = Envelope::decode(&burn.hook_data)?;
    let recipient = envelope.recipient;
    if recipient == EvmAddress::ZERO
        || [
            destination.swapic,
            destination.runner,
            destination.cctp_receiver,
        ]
        .contains(&recipient)
    {
        return Err(RelayError::Arrival(
            "recipient is zero or one of Swapic's own contracts",
        ));
    }
    let action = match envelope.action {
        Action::Swap | Action::CkDeposit => {
            let receiver = destination.cctp_receiver.to_word();
            if burn.mint_recipient != receiver || burn.destination_caller != receiver {
                return Err(RelayError::Burn(
                    "of a Swap or a CkDeposit is not minted to the destination CctpReceiver and called by it",
                ));
            }
            if envelope.min_gas == 0 {
                return Err(RelayError::Arrival("sets no minGas"));
            }
            if envelope.action == Action::Swap {
                if envelope.min_out == U256::ZERO || envelope.token_out == EvmAddress::ZERO {
                    return Err(RelayError::Arrival("of a Swap names no tokenOut or minOut"));
                }
                GasAction::CompleteSwap
            } else {
                GasAction::CompleteCk
            }
        }
        Action::Deliver => {
            if burn.mint_recipient != recipient.to_word() {
                return Err(RelayError::Burn(
                    "of a Deliver is not minted to the envelope's recipient",
                ));
            }
            if burn.destination_caller != [0; 32]
                && burn.destination_caller != chains.canister.to_word()
            {
                return Err(RelayError::Burn(
                    "of a Deliver may be received by anyone or by the canister only",
                ));
            }
            GasAction::RelayDeliver
        }
    };
    Ok(Arrival {
        chain_id,
        action,
        recipient,
    })
}

/// Every check on the order and its authorization that needs no chain: pure, so each rule
/// is a unit test. `requested_gas` is the limit the quoter charged the fee on.
pub fn check(
    order: &Order,
    auth: &GaslessAuth,
    requested_gas: u64,
    chains: &Chains,
    now_s: u64,
    window_s: u64,
) -> Result<Relay, RelayError> {
    let chain_id = chains.chain_id;
    let source = chains
        .evm_chains
        .get(chain_id)
        .ok_or(RelayError::UnknownChain(chain_id))?;
    let order_id = swapic::order_id(order, chain_id, source.swapic);
    check_auth(auth, order_id, now_s)?;

    if order.payer.is_zero() {
        return Err(RelayError::Order("payer is the zero address"));
    }
    let token_in = swapic::evm(order.tokenIn);
    if token_in == EvmAddress::ZERO || token_in == NATIVE {
        return Err(RelayError::Order(
            "pays in ether or the zero address, which no gasless door pulls",
        ));
    }
    if order.amountIn.is_zero() {
        return Err(RelayError::Order("pays nothing"));
    }
    let earliest = now_s.saturating_add(RELAY_MARGIN_S);
    let latest = now_s.saturating_add(window_s);
    if order.deadline <= U256::from(earliest) || order.deadline > U256::from(latest) {
        return Err(RelayError::Deadline {
            deadline: order.deadline,
            earliest,
            latest,
        });
    }

    let mut fees = Vec::new();
    let mut burns = Vec::new();
    let mut eco = false;
    for step in &order.steps {
        if let Some((to, amount)) = swapic::fee_transfer_of(step, source.usdc) {
            if to == chains.canister {
                fees.push(amount);
            }
        }
        if let Some(burn) = CctpBurn::of_step(step, source.token_messenger) {
            burns.push(burn.map_err(|_| RelayError::Burn("is not a depositForBurnWithHook"))?);
        }
        eco |= swapic::selector_of(step) == Some(PUBLISH_AND_FUND);
    }
    let fee = match fees.as_slice() {
        [] => return Err(RelayError::NoFeeStep),
        [fee] => TokenAmount::from_be_bytes(fee.to_be_bytes::<32>()),
        more => return Err(RelayError::FeeSteps(more.len())),
    };
    let (action, arrival) = match burns.as_slice() {
        [] if eco => (GasAction::RelayEco, None),
        [] => (GasAction::RelayLocal, None),
        [_] if eco => return Err(RelayError::TwoRails),
        [burn] => (
            GasAction::RelayCctp,
            Some(arrival_of(burn, &source, chains)?),
        ),
        more => return Err(RelayError::Burns(more.len())),
    };

    let mut needed = source.min_network_fee.get(action);
    if let Some(arrival) = arrival {
        let destination = chains
            .evm_chains
            .get(arrival.chain_id)
            .expect("BUG: arrival_of found the destination in this table");
        needed = needed
            .checked_add(destination.min_network_fee.get(arrival.action))
            .unwrap_or(TokenAmount::MAX);
    }
    if fee < needed {
        return Err(RelayError::FeeBelowMinimum { paid: fee, needed });
    }
    let gas_limit = signing_gas(chain_id, &source, action, requested_gas)?;
    Ok(Relay {
        order_id,
        action,
        fee,
        arrival,
        gas_limit,
    })
}

/// `relay_gasless`: answers the signed transaction's id in `signed_txs`. Its status is
/// `relay_status(order_id)`.
pub async fn relay_gasless(
    chain_id: ChainId,
    order_bytes: &[u8],
    auth_bytes: &[u8],
    gas_limit: u64,
) -> Result<u64, String> {
    // the role and the halt before anything else, and before any outcall
    require_watcher().map_err(|e| format!("{e:?}"))?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    let order = swapic::decode_order(order_bytes).ok_or(RelayError::NotAnOrder.to_string())?;
    let auth = swapic::decode_auth(auth_bytes).ok_or(RelayError::NotAnAuth.to_string())?;
    // derived once by the deploy and read from stable memory after that
    let canister = ecdsa::canister_address().await.map_err(|e| e.to_string())?;
    let config = config::get();
    let settings = flow_settings::get();
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    let chains = Chains {
        chain_id,
        evm_chains: &config.evm_chains,
        domains: &config.cctp_domains,
        canister,
    };
    let relay = check(
        &order,
        &auth,
        gas_limit,
        &chains,
        now_s,
        settings.relay_window.as_secs(),
    )
    .map_err(|e| e.to_string())?;
    let payout_recipient =
        (!order.payout.token.is_zero()).then(|| swapic::evm(order.payout.recipient));
    let sanctioned = [
        Some(swapic::evm(order.payer)),
        payout_recipient,
        relay.arrival.map(|a| a.recipient),
    ]
    .into_iter()
    .flatten()
    .any(|address| is_sanctioned(&types::Address::from(address)));
    if sanctioned {
        return Err("the payer or a recipient is sanctioned".into());
    }

    let key = QuoteHash::new(relay.order_id.into_bytes());
    if let Some(id) = tx::signed_once(key)? {
        return Ok(id);
    }
    let _busy = Busy::take(key).ok_or("the order is being signed")?;
    let swapic_address = config
        .evm_chains
        .get(chain_id)
        .expect("BUG: check found the chain in this table")
        .swapic;
    let data = swapic::run_for_calldata(&order, &auth);
    if config.simulate_before_sign {
        let (simulated, _) =
            evm_reads::simulate_and_estimate(chain_id, canister, swapic_address, &data)
                .await
                .map_err(|e| e.to_string())?;
        if let Simulated::Reverted(message) = simulated {
            return Err(format!("runFor reverts in simulation: {message}"));
        }
        // the halt and a concurrent relay can land while the read was out
        require_not_halted().map_err(|e| format!("{e:?}"))?;
        if let Some(id) = tx::signed_once(key)? {
            return Ok(id);
        }
    }
    let signed = tx::create_and_send(
        TxPurpose::Relay(key),
        chain_id,
        swapic_address,
        Wei::ZERO,
        data,
        relay.gas_limit,
        relay.action,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(signed.id.get())
}
