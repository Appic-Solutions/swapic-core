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
use std::fmt;
use thiserror::Error;
use types::config::{ChainTable, EvmChain, GasAction};
use types::events::TxPurpose;
use types::swapic::{
    self, Action, CctpBurn, EcoArrival, EcoIntent, Envelope, EnvelopeError, GaslessAuth, KnownStep,
    KyberSwap, Order, OrderId, Step, StepError, DOOR_AUTH_3009, DOOR_PERMIT2, DOOR_PERMIT_2612,
    KYBER_EXECUTOR, KYBER_ROUTER, NATIVE,
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
    #[error(
        "a gas limit of {requested} is above the {max} a {action:?} may have: the canister \
         never signs below the gas the fee was charged on"
    )]
    GasAboveMax {
        action: GasAction,
        requested: u64,
        max: GasAmount,
    },
    #[error("chain {chain_id} gives a {action:?} no gas")]
    NoGasLimit {
        chain_id: ChainId,
        action: GasAction,
    },
    #[error("{0} is sanctioned")]
    Sanctioned(Party),
    #[error(
        "the authorization does not recover to the payer: a gasless order is signed by the \
         payer's own key, and a contract wallet's ERC-1271 signature is not supported"
    )]
    ContractPayerUnsupported,
    #[error(
        "step {0} is none of the shapes the canister reads: the fee, a CCTP burn, an Eco \
         intent on Eco's Portal, a KyberSwap swap paying the Runner"
    )]
    UnknownStep(usize),
    #[error("the swap {0}")]
    Swap(&'static str),
    #[error("the Eco intent's destination {0} is no chain the canister signs on")]
    EcoUnknownDestination(u64),
    #[error(
        "the order names no recipient the canister can read: no CCTP burn, no Eco intent and no payout"
    )]
    NoRecipient,
    #[error("the order publishes {0} Eco intents")]
    EcoIntents(usize),
    #[error("step {index} is not read as the Runner will run it: {error}")]
    Step { index: usize, error: StepError },
    #[error("the Eco intent {0}")]
    Eco(&'static str),
}

/// What the order's checks read besides the order: the chains the canister signs on and
/// their CCTP domains, and the canister's own address, which the fee step pays.
pub struct Chains<'a> {
    pub chain_id: ChainId,
    pub evm_chains: &'a ChainTable<EvmChain>,
    pub domains: &'a ChainTable<CctpDomain>,
    pub canister: EvmAddress,
    /// Eco's Portal, the one target an Eco intent is published on. None: no Eco order is
    /// relayed.
    pub eco_portal: Option<EvmAddress>,
}

/// A party an order or a message pays, screened against the sanctions set: an EVM
/// address, or the ICP principal a CkDeposit credits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    Evm(EvmAddress),
    Icp(candid::Principal),
}

impl fmt::Display for Party {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Party::Evm(address) => write!(f, "{address}"),
            Party::Icp(principal) => write!(f, "{principal}"),
        }
    }
}

impl Party {
    /// Whether the sanctions set holds this party. A principal is held as its text.
    fn is_sanctioned(&self) -> bool {
        match self {
            Party::Evm(address) => is_sanctioned(&types::Address::from(*address)),
            // a principal's text is at most 63 bytes, far inside the bound; a text that
            // would not fit is refused as if it were listed
            Party::Icp(principal) => principal
                .to_text()
                .parse::<types::Address>()
                .map_or(true, |text| is_sanctioned(&text)),
        }
    }
}

/// The parties an envelope pays: its recipient, and for a CkDeposit the principal it
/// credits, which must read as one.
pub fn envelope_parties(envelope: &Envelope) -> Result<Vec<Party>, &'static str> {
    let mut parties = vec![Party::Evm(envelope.recipient)];
    if envelope.action == Action::CkDeposit {
        let principal = swapic::ck_principal(envelope.principal)
            .ok_or("names a principal the canister cannot read")?;
        parties.push(Party::Icp(principal));
    }
    Ok(parties)
}

/// What the canister signs for an order, read from the order.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Every party the order pays: the payer and each recipient, the principal of a
    /// CkDeposit included. None of them may be sanctioned.
    pub parties: Vec<Party>,
}

/// The first of `parties` the sanctions set holds, if any.
pub fn sanctioned_party(parties: &[Party]) -> Option<Party> {
    parties.iter().copied().find(Party::is_sanctioned)
}

/// The destination side of a CCTP order, as its burn seals it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub chain_id: ChainId,
    pub action: GasAction,
    pub recipient: EvmAddress,
    /// Who the arrival pays: the recipient, and a CkDeposit's principal.
    pub parties: Vec<Party>,
}

/// The limit a transaction for `action` on `chain` is signed with: exactly the one asked
/// for, which is the limit the quoter charged the fee on. A limit above the chain's
/// maximum for the action is refused, never lowered: a transaction signed below what it
/// was charged for could run out of gas. A release is the one exception: anyone may send
/// it, the watcher asks for it with the arrival's limit, and it is signed at the maximum.
pub fn signing_gas(
    chain_id: ChainId,
    chain: &EvmChain,
    action: GasAction,
    requested: u64,
) -> Result<GasAmount, RelayError> {
    if requested < MIN_GAS_LIMIT {
        return Err(RelayError::GasLimitTooLow(requested));
    }
    let max = chain.gas_limit.get(action);
    let requested_gas = GasAmount::from(requested);
    let gas = if requested_gas <= max {
        requested_gas
    } else if action == GasAction::Release {
        max
    } else {
        return Err(RelayError::GasAboveMax {
            action,
            requested,
            max,
        });
    };
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
    // what must still hold when the relay lands, a margin after the canister's clock
    let landing = U256::from(now_s.saturating_add(RELAY_MARGIN_S));
    match auth.door {
        DOOR_AUTH_3009 => {
            if auth.nonce != id.as_nonce() {
                return Err(RelayError::WrongOrderId(id));
            }
            if auth.validAfter > now {
                return Err(RelayError::Authorization("is not valid yet"));
            }
            if auth.validBefore < landing {
                return Err(RelayError::Authorization("expires before the relay lands"));
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
            if auth.deadline < landing {
                return Err(RelayError::Authorization(
                    "Permit2 permit expires before the relay lands",
                ));
            }
            if auth.signature.is_empty() {
                return Err(RelayError::Authorization("carries no Permit2 signature"));
            }
        }
        door => return Err(RelayError::UnknownDoor(door)),
    }
    Ok(())
}

/// Half the secp256k1 group order: a signature whose `s` is above it is the malleable
/// twin every verifier the doors use refuses (EIP-2), so the canister refuses it too.
const HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// The payer's own authorization, verified here with no outcall, so the canister signs
/// nothing a door will refuse on chain at its cost. Each door's signature must recover,
/// over what the door verifies, to the payer:
/// - the 3009 door: USDC's `ReceiveWithAuthorization` under the chain's USDC's domain, so
///   the order must pay in that USDC;
/// - the 2612 door: the payer's `QuoteAcceptance` of the order id under Swapic's domain;
/// - the Permit2 door: Permit2's `PermitWitnessTransferFrom` with the order id as witness.
///
/// A payer whose signature does not recover, a contract wallet signing by ERC-1271
/// included, is refused for gasless.
pub fn authorize(
    order: &Order,
    auth: &GaslessAuth,
    id: OrderId,
    chain_id: ChainId,
    source: &EvmChain,
) -> Result<(), RelayError> {
    let (digest, signature) = match auth.door {
        DOOR_AUTH_3009 => {
            if swapic::evm(order.tokenIn) != source.usdc {
                return Err(RelayError::Authorization(
                    "of the 3009 door pulls only the chain's USDC",
                ));
            }
            let digest = swapic::receive_with_authorization_digest(
                order,
                auth,
                id,
                chain_id,
                source.usdc,
                source.swapic,
            );
            (digest, auth.signature.as_ref())
        }
        DOOR_PERMIT_2612 => (
            swapic::acceptance_digest(order, chain_id, source.swapic),
            auth.acceptance.as_ref(),
        ),
        DOOR_PERMIT2 => (
            swapic::permit2_digest(order, auth, id, chain_id, source.swapic),
            auth.signature.as_ref(),
        ),
        door => return Err(RelayError::UnknownDoor(door)),
    };
    if signature.len() == 65 && signature[32..64] > HALF_ORDER[..] {
        return Err(RelayError::Authorization("signature is not in low-s form"));
    }
    match ecdsa::recover_signer(digest, signature) {
        Some(signer) if signer == swapic::evm(order.payer) => Ok(()),
        _ => Err(RelayError::ContractPayerUnsupported),
    }
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
        parties: envelope_parties(&envelope).map_err(RelayError::Arrival)?,
    })
}

/// The parties an Eco intent pays, read as the destination will run its route: the
/// destination is a chain the canister signs on, the route carries that chain's USDC and
/// approves and fills that chain's Runner, the reward pays the source chain's USDC, and an
/// unfilled intent refunds its creator, who is the payer (rule 19).
fn eco_parties(
    intent: &EcoIntent,
    payer: EvmAddress,
    source: &EvmChain,
    chains: &Chains,
) -> Result<Vec<Party>, RelayError> {
    if intent.creator != payer {
        return Err(RelayError::Eco("refunds to someone other than the payer"));
    }
    let destination = ChainId::new(intent.destination);
    let chain = chains
        .evm_chains
        .get(destination)
        .filter(|_| destination != chains.chain_id)
        .ok_or(RelayError::EcoUnknownDestination(intent.destination))?;
    if intent.token != chain.usdc {
        return Err(RelayError::Eco("carries other than the destination's USDC"));
    }
    if intent.reward_token != source.usdc {
        return Err(RelayError::Eco("rewards other than the chain's USDC"));
    }
    let ours = [chain.swapic, chain.runner, chain.cctp_receiver];
    let parties = match intent.arrival {
        EcoArrival::Deliver { recipient } => {
            if recipient == EvmAddress::ZERO || ours.contains(&recipient) {
                return Err(RelayError::Eco(
                    "delivers to zero or one of Swapic's own contracts",
                ));
            }
            vec![Party::Evm(recipient)]
        }
        EcoArrival::Fill { runner, envelope } => {
            if runner != chain.runner {
                return Err(RelayError::Eco(
                    "fills another Runner than the destination's",
                ));
            }
            if envelope.action == Action::Deliver {
                return Err(RelayError::Eco(
                    "fills a Deliver envelope, which fill refuses",
                ));
            }
            if envelope.recipient == EvmAddress::ZERO || ours.contains(&envelope.recipient) {
                return Err(RelayError::Eco(
                    "fills for zero or one of Swapic's own contracts",
                ));
            }
            envelope_parties(&envelope).map_err(RelayError::Eco)?
        }
    };
    Ok(parties)
}

/// A swap step the canister reads in full but for its route: KyberSwap's router, called
/// with the Runner's delta of the token the swap sells, handing it only to the executor
/// it calls, paying no fees, and paying its output to the Runner, where the plan's next
/// step or its payout takes it.
fn check_swap(swap: &KyberSwap, step: &Step, source: &EvmChain) -> Result<(), RelayError> {
    let sold = swapic::evm(step.token);
    if swapic::evm(step.target) != KYBER_ROUTER {
        return Err(RelayError::Swap("is not on KyberSwap's router"));
    }
    if sold == EvmAddress::ZERO || swap.src_token != sold {
        return Err(RelayError::Swap("does not sell the token the step spends"));
    }
    if swap.call_target != KYBER_EXECUTOR || swap.src_receivers != [swap.call_target] {
        return Err(RelayError::Swap(
            "hands its input to other than KyberSwap's executor",
        ));
    }
    if !swap.fee_receivers.is_empty() || swap.permit {
        return Err(RelayError::Swap("pays fees or carries a permit"));
    }
    if swap.dst_receiver != source.runner {
        return Err(RelayError::Swap("pays its output to other than the Runner"));
    }
    Ok(())
}

/// What an order does, read in full: the fee steps it pays the canister, its rail and
/// arrival, and every party it pays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub fees: Vec<U256>,
    pub action: GasAction,
    pub arrival: Option<Arrival>,
    pub parties: Vec<Party>,
}

/// An order's plan, read as the Runner will run it, or the refusal: every step is one of
/// the shapes `read_step` reads exactly (a fixed USDC transfer to the canister, a CCTP
/// burn, an Eco intent on Eco's Portal, a KyberSwap swap paying the Runner), at most one
/// burn or one intent, and a recipient the canister can read. Pure, and true of an order
/// in either gas mode.
pub fn read_plan(order: &Order, chains: &Chains) -> Result<Plan, RelayError> {
    let chain_id = chains.chain_id;
    let source = chains
        .evm_chains
        .get(chain_id)
        .ok_or(RelayError::UnknownChain(chain_id))?;
    // every step is one of the shapes the canister reads, or the order is refused: a
    // step it cannot read could pay someone the screen never sees
    let mut fees = Vec::new();
    let mut burns = Vec::new();
    let mut intents = Vec::new();
    for (index, step) in order.steps.iter().enumerate() {
        let read = match swapic::read_step(step, source.usdc, source.token_messenger) {
            Ok(read) => read,
            Err(StepError::Unknown) => return Err(RelayError::UnknownStep(index)),
            Err(error) => return Err(RelayError::Step { index, error }),
        };
        match read {
            KnownStep::Transfer { to, amount } => {
                // a fixed transfer to anyone but the canister pays someone the plan does
                // not need
                if to != chains.canister {
                    return Err(RelayError::UnknownStep(index));
                }
                fees.push(amount);
            }
            KnownStep::Burn(burn) => burns.push(burn),
            KnownStep::Eco { intent, .. } => {
                if chains.eco_portal != Some(swapic::evm(step.target)) {
                    return Err(RelayError::Eco("is not published on Eco's Portal"));
                }
                intents.push(intent);
            }
            KnownStep::Swap(swap) => check_swap(&swap, step, &source)?,
        }
    }

    // every party the order pays, each to be screened: the canister signs only a plan
    // whose recipient it can read
    let payer = swapic::evm(order.payer);
    let mut parties = vec![Party::Evm(payer)];
    if !order.payout.token.is_zero() {
        parties.push(Party::Evm(swapic::evm(order.payout.recipient)));
    }
    let (action, arrival) = match (burns.as_slice(), intents.as_slice()) {
        ([], []) if order.payout.token.is_zero() => return Err(RelayError::NoRecipient),
        ([], []) => (GasAction::RelayLocal, None),
        ([], [intent]) => {
            parties.extend(eco_parties(intent, payer, &source, chains)?);
            (GasAction::RelayEco, None)
        }
        ([burn], []) => {
            let arrival = arrival_of(burn, &source, chains)?;
            parties.extend(arrival.parties.iter().copied());
            (GasAction::RelayCctp, Some(arrival))
        }
        ([_, ..], [_, ..]) => return Err(RelayError::TwoRails),
        (more, []) => return Err(RelayError::Burns(more.len())),
        ([], more) => return Err(RelayError::EcoIntents(more.len())),
    };
    Ok(Plan {
        fees,
        action,
        arrival,
        parties,
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

    let plan = read_plan(order, chains)?;
    let fee = match plan.fees.as_slice() {
        [] => return Err(RelayError::NoFeeStep),
        [fee] => TokenAmount::from_be_bytes(fee.to_be_bytes::<32>()),
        more => return Err(RelayError::FeeSteps(more.len())),
    };
    let Plan {
        action,
        arrival,
        parties,
        ..
    } = plan;

    let mut needed = source.min_network_fee.get(action);
    if let Some(arrival) = &arrival {
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
        parties,
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
        eco_portal: config.eco_portal,
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
    let source = config
        .evm_chains
        .get(chain_id)
        .expect("BUG: check found the chain in this table");
    authorize(&order, &auth, relay.order_id, chain_id, &source).map_err(|e| e.to_string())?;
    if let Some(party) = sanctioned_party(&relay.parties) {
        return Err(RelayError::Sanctioned(party).to_string());
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
                // the kind only: an outcall's text can carry the provider's url and key
                .map_err(|e| format!("the simulation was not read: {:?}", e.kind()))?;
        if let Simulated::Reverted(_) = simulated {
            return Err("runFor reverts in simulation".into());
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
