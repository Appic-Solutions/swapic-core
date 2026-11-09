//! The gasless relay, design C: a user's signed order, checked, signed once as
//! `Swapic.runFor` from this canister's address (Swapic's `canister()`, the only caller
//! `runFor` admits), and handed to the watcher through `signed_txs`.
//!
//! Money-first: the role and the halt come before anything else, the source chain's
//! reading the call carries right after them (`take_reading`); the order id is
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
use crate::storage::chain_data;
use crate::storage::sanctions::is_sanctioned;
use crate::storage::{config, flow_settings};
use crate::tx;
use alloy_primitives::U256;
use settlement_api::types::chain_data::ChainData;
use std::fmt;
use thiserror::Error;
use types::config::{AcrossFee, ChainTable, EvmChain, GasAction};
use types::swapic::across::{self, AcrossDeposit, MAX_MIN_GAS};
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
    #[error(
        "step {0} pays the fee before the order holds the chain's USDC: it pays in another \
         token and no swap into USDC comes first"
    )]
    FeeBeforeUsdc(usize),
    #[error(
        "the fee and the payout's least amount, {owed}, are more than the swaps into USDC \
         buy at their least, {bought}"
    )]
    FeeAboveSwap { owed: U256, bought: U256 },
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
    #[error("the Across deposit breaks rule {0}")]
    Across(&'static str),
    #[error("the order deposits on Across {0} times, and one fee pays for one deposit")]
    AcrossDeposits(usize),
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
    /// AX13 and AX14's bounds. None: no order with an Across step is relayed.
    pub across_fee: Option<&'a AcrossFee>,
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
    let max = chain
        .gas_limit
        .get(action)
        .ok_or(RelayError::NoGasLimit { chain_id, action })?;
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
/// twin that USDC's ECRecover and OpenZeppelin's `tryRecover` refuse (EIP-2).
const HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// The verifier a door's signature meets on chain, whose rules the canister applies
/// exactly: a signature it accepts and the verifier refuses costs a revert, and one it
/// refuses and the verifier accepts refuses a payer for nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verifier {
    /// USDC FiatToken v2.2's `ECRecover` (the 3009 door, through Swapic's 65-byte split)
    /// and OpenZeppelin 5.1's `ECDSA.tryRecover` (the 2612 door's acceptance): 65 bytes
    /// `r ++ s ++ v`, `v` 27 or 28, `s` in the low half, a signer that is not zero.
    LowS,
    /// Permit2's `SignatureVerification`: 65 bytes, or EIP-2098's 64 (`r`, then `s` with
    /// `v`'s parity in its top bit), and `ecrecover` as it is: `v` 27 or 28, `s` in either
    /// half.
    Permit2,
}

/// The signer of `signature` over `digest`, read as `verifier` reads it.
pub(crate) fn recover_as(
    verifier: Verifier,
    digest: [u8; 32],
    signature: &[u8],
) -> Result<Option<EvmAddress>, RelayError> {
    let word = |at: usize| -> [u8; 32] {
        signature[at..at + 32]
            .try_into()
            .expect("BUG: the length was checked")
    };
    let (r, s, v) = match (verifier, signature.len()) {
        (_, 65) => (word(0), word(32), signature[64]),
        (Verifier::Permit2, 64) => {
            let mut s = word(32);
            let parity = s[0] >> 7;
            s[0] &= 0x7f;
            (word(0), s, 27 + parity)
        }
        (Verifier::LowS, _) => return Err(RelayError::Authorization("signature is not 65 bytes")),
        (Verifier::Permit2, _) => {
            return Err(RelayError::Authorization("signature is not 65 or 64 bytes"))
        }
    };
    if v != 27 && v != 28 {
        return Err(RelayError::Authorization("signature's v is not 27 or 28"));
    }
    if verifier == Verifier::LowS && s > HALF_ORDER {
        return Err(RelayError::Authorization("signature is not in low-s form"));
    }
    Ok(ecdsa::ecrecover(digest, v, r, s))
}

/// The payer's own authorization, verified here with no outcall, so the canister signs
/// nothing a door will refuse on chain at its cost. Each door's signature must recover,
/// read exactly as the door's verifier reads it ([`Verifier`]), over what the door
/// verifies, every field taken from the parsed order and the recomputed id, to the payer:
/// - the 3009 door: USDC's `ReceiveWithAuthorization` from the payer to Swapic of
///   `amountIn`, nonce the order id, under the chain's USDC's domain, so the order must pay
///   in that USDC;
/// - the 2612 door: the payer's `QuoteAcceptance` of the order id under Swapic's domain on
///   this chain;
/// - the Permit2 door: Permit2's `PermitWitnessTransferFrom` of `amountIn` of `tokenIn`,
///   Swapic the spender, the order id the witness.
///
/// A payer whose signature does not recover, a contract wallet signing by ERC-1271
/// included, is refused for gasless. What only the chain knows is not read here: USDC's
/// authorization state and Permit2's nonce bitmap, the 2612 door's allowance, and whether
/// the payer has code (an EIP-7702 account, whose 3009 and Permit2 verifiers then ask
/// ERC-1271 instead); the submit API's simulation from the canister's address (I-3.3), and
/// `simulate_before_sign` when it is on, meet those.
pub fn authorize(
    order: &Order,
    auth: &GaslessAuth,
    id: OrderId,
    chain_id: ChainId,
    source: &EvmChain,
) -> Result<(), RelayError> {
    let (verifier, digest, signature) = match auth.door {
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
                source.usdc_domain(),
                source.swapic,
            );
            (Verifier::LowS, digest, auth.signature.as_ref())
        }
        DOOR_PERMIT_2612 => (
            Verifier::LowS,
            swapic::acceptance_digest(order, chain_id, source.swapic),
            auth.acceptance.as_ref(),
        ),
        DOOR_PERMIT2 => (
            Verifier::Permit2,
            swapic::permit2_digest(order, auth, id, chain_id, source.swapic),
            auth.signature.as_ref(),
        ),
        door => return Err(RelayError::UnknownDoor(door)),
    };
    match recover_as(verifier, digest, signature)? {
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
/// with the Runner's delta of the token the swap sells, calling KyberSwap's executor with
/// the route, handing the input to the receivers the route starts at (the executor, or an
/// old v2-style pool itself, which KyberSwap's router pays directly), at least one and
/// none of them the zero address, one of Swapic's own contracts here or the canister,
/// paying no fees, carrying no permit, and paying its output to the Runner, where the
/// plan's next step or its payout takes it.
///
/// The input's receivers need no closer reading, and its amounts none at all: the Runner
/// approves the router for exactly the step's delta and reverts on anything left of it
/// (`LeftOver`), so the receivers share exactly what the step hands over; and whoever
/// receives it, what comes back is held by KyberSwap's router (at least `minReturnAmount`
/// of the output to `dst_receiver`, the Runner, or the call reverts) and by the Runner's
/// signed minimums (each later step's `min` and the payout's `minOut`, or the whole order
/// reverts). A receiver that keeps the input can make the order revert, never pay anyone
/// less than the order was signed for.
fn check_swap(
    swap: &KyberSwap,
    step: &Step,
    source: &EvmChain,
    canister: EvmAddress,
) -> Result<(), RelayError> {
    let sold = swapic::evm(step.token);
    if swapic::evm(step.target) != KYBER_ROUTER {
        return Err(RelayError::Swap("is not on KyberSwap's router"));
    }
    if sold == EvmAddress::ZERO || swap.src_token != sold {
        return Err(RelayError::Swap("does not sell the token the step spends"));
    }
    if swap.call_target != KYBER_EXECUTOR {
        return Err(RelayError::Swap("calls other than KyberSwap's executor"));
    }
    let ours = [
        EvmAddress::ZERO,
        source.swapic,
        source.runner,
        source.cctp_receiver,
        canister,
    ];
    if swap.src_receivers.is_empty() {
        return Err(RelayError::Swap("hands its input to no receiver"));
    }
    if swap
        .src_receivers
        .iter()
        .any(|receiver| ours.contains(receiver))
    {
        return Err(RelayError::Swap(
            "hands its input to the zero address, one of Swapic's own contracts or the canister",
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

/// Where an order's money ends up, as the canister reads it from the order's own bytes: the
/// chain, the recipient, the token and the least of it the recipient gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub chain: ChainId,
    pub recipient: EvmAddress,
    pub token: EvmAddress,
    pub min_out: TokenAmount,
}

/// What an order does, read in full: the fee steps it pays the canister, its rail and
/// arrival, every party it pays, and its final delivery when it has exactly one the canister
/// reads (its payout on its own chain; a CCTP burn's Swap envelope; an Eco intent's delivery
/// or its fill's Swap envelope).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub fees: Vec<U256>,
    pub action: GasAction,
    pub arrival: Option<Arrival>,
    pub parties: Vec<Party>,
    pub delivery: Option<Delivery>,
    /// The order's one Eco intent, as its step reads.
    pub intent: Option<EcoIntent>,
}

/// A Swap envelope's delivery on `chain`: its recipient gets at least its `min_out` of its
/// `token_out`. None for any other envelope.
fn envelope_delivery(envelope: &Envelope, chain: ChainId) -> Option<Delivery> {
    (envelope.action == Action::Swap).then(|| Delivery {
        chain,
        recipient: envelope.recipient,
        token: envelope.token_out,
        min_out: TokenAmount::from_u256(envelope.min_out),
    })
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
    // each intent's step: the least delta the Runner gives it, and the fee Eco keeps
    let mut intent_steps = Vec::new();
    // the chain's USDC the Runner holds for a fee step: the order's own when it pays in
    // USDC, else what the swaps into USDC before it buy at their least (KyberSwap's
    // `minReturnAmount`); a fee paid from those swaps, and the least of them it left
    let pays_in_usdc = swapic::evm(order.tokenIn) == source.usdc;
    let mut swapped_into_usdc = false;
    let mut bought = U256::ZERO;
    let mut paid_from_swaps = None;
    // each Across deposit and its step's index (BSC design 6.2.6)
    let mut deposits = Vec::new();
    for (index, step) in order.steps.iter().enumerate() {
        // a step to this chain's Across periphery is read as a deposit, and as nothing else
        if let Some(pins) = source
            .across
            .filter(|pins| swapic::evm(step.target) == pins.periphery)
        {
            let deposit = across::read_across_step(step, &pins)
                .map_err(|error| RelayError::Step { index, error })?;
            deposits.push((index, deposit));
            continue;
        }
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
                // a fee step before the order holds USDC would pay from no one's USDC: on
                // chain it reverts, at the canister's cost
                if !pays_in_usdc {
                    if !swapped_into_usdc {
                        return Err(RelayError::FeeBeforeUsdc(index));
                    }
                    paid_from_swaps = Some(bought);
                }
                fees.push(amount);
            }
            KnownStep::Burn(burn) => burns.push((burn, step.min)),
            KnownStep::Eco { intent, fee } => {
                if chains.eco_portal != Some(swapic::evm(step.target)) {
                    return Err(RelayError::Eco("is not published on Eco's Portal"));
                }
                intents.push(intent);
                intent_steps.push((step.min, fee));
            }
            KnownStep::Swap(swap) => {
                check_swap(&swap, step, &source, chains.canister)?;
                if swap.dst_token == source.usdc {
                    swapped_into_usdc = true;
                    bought = bought.saturating_add(swap.min_return);
                }
            }
        }
    }
    if !order.payout.token.is_zero() && order.payout.minOut.is_zero() {
        return Err(RelayError::Order("pays out with no least amount"));
    }
    // a fee paid from what swaps bought, with the rest paid out in USDC, must leave the
    // payout its least amount when the swaps buy only their own least, or the order can
    // revert while every swap keeps its word
    if let Some(bought) = paid_from_swaps {
        if swapic::evm(order.payout.token) == source.usdc {
            let owed = fees
                .iter()
                .fold(order.payout.minOut, |owed, fee| owed.saturating_add(*fee));
            if owed > bought {
                return Err(RelayError::FeeAboveSwap { owed, bought });
            }
        }
    }

    // every party the order pays, each to be screened: the canister signs only a plan
    // whose recipient it can read
    let payer = swapic::evm(order.payer);
    let mut parties = vec![Party::Evm(payer)];
    if !order.payout.token.is_zero() {
        parties.push(Party::Evm(swapic::evm(order.payout.recipient)));
    }
    let payout = (!order.payout.token.is_zero()).then(|| Delivery {
        chain: chain_id,
        recipient: swapic::evm(order.payout.recipient),
        token: swapic::evm(order.payout.token),
        min_out: TokenAmount::from_u256(order.payout.minOut),
    });
    // an Across order (BSC design 6.2.6): one deposit, the last step, no payout, no other
    // rail; the relayer pays the destination, so there is no arrival action
    if !deposits.is_empty() {
        if !burns.is_empty() || !intents.is_empty() {
            return Err(RelayError::TwoRails);
        }
        let [(index, deposit)] = deposits.as_slice() else {
            return Err(RelayError::AcrossDeposits(deposits.len()));
        };
        if *index + 1 != order.steps.len() {
            return Err(RelayError::Across("AX11"));
        }
        if !order.payout.token.is_zero() {
            return Err(RelayError::Across("AX10"));
        }
        let (delivery, more) = across_delivery(deposit, order, &source, chains)?;
        parties.extend(more);
        return Ok(Plan {
            fees,
            action: GasAction::RelayAcross,
            arrival: None,
            parties,
            delivery: Some(delivery),
            intent: None,
        });
    }
    let (action, arrival, delivery) = match (burns.as_slice(), intents.as_slice()) {
        ([], []) if order.payout.token.is_zero() => return Err(RelayError::NoRecipient),
        ([], []) => (GasAction::RelayLocal, None, payout),
        ([], [intent]) => {
            parties.extend(eco_parties(intent, payer, &source, chains)?);
            let destination = ChainId::new(intent.destination);
            let delivered = match intent.arrival {
                // the route carries the Runner's delta less Eco's fee, and the Runner gives
                // the step at least its `min`
                EcoArrival::Deliver { recipient } => {
                    let (min, fee) = intent_steps[0];
                    Some(Delivery {
                        chain: destination,
                        recipient,
                        token: intent.token,
                        min_out: TokenAmount::from_u256(min.saturating_sub(fee)),
                    })
                }
                EcoArrival::Fill { envelope, .. } => envelope_delivery(&envelope, destination),
            };
            // an intent and a payout too are two deliveries: none is the order's
            let delivery = if payout.is_some() { None } else { delivered };
            (GasAction::RelayEco, None, delivery)
        }
        ([(burn, min)], []) => {
            let arrival = arrival_of(burn, &source, chains)?;
            parties.extend(arrival.parties.iter().copied());
            // a Deliver mints the destination's USDC to its recipient: at least what the
            // Runner gives the burn less the most Circle may keep (CCTP payouts)
            let usdc = chains
                .evm_chains
                .get(arrival.chain_id)
                .map(|destination| destination.usdc);
            let delivered = Envelope::decode(&burn.hook_data).ok().and_then(|envelope| {
                match (envelope.action, usdc) {
                    (Action::Deliver, Some(usdc)) => Some(Delivery {
                        chain: arrival.chain_id,
                        recipient: envelope.recipient,
                        token: usdc,
                        min_out: TokenAmount::from_u256(min.saturating_sub(burn.max_fee)),
                    }),
                    _ => envelope_delivery(&envelope, arrival.chain_id),
                }
            });
            let delivery = if payout.is_some() { None } else { delivered };
            (GasAction::RelayCctp, Some(arrival), delivery)
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
        delivery,
        intent: intents.first().copied(),
    })
}

/// AX8: a deposit's `fillDeadline` is between these after the order's deadline.
const FILL_AFTER_LEAST_S: u64 = 300;
const FILL_AFTER_MOST_S: u64 = 900;
/// AX9: an exclusive relayer's turn, an offset from the deposit's block, at most this.
const MAX_EXCLUSIVITY_S: u32 = 300;
/// AX12: the order ends within this of the quote's time.
const QUOTE_LIFE_S: u64 = 1_800;

/// An Across deposit's delivery and the parties it pays, or the rule it breaks (BSC design
/// 7.4, AX5 to AX9, AX12 to AX14, AM1 to AM4; AX1 to AX4 are the step reader's, AX10 and AX11
/// the plan's): the payer's refund and speed-ups, another chain the canister holds Across's
/// pins for, that chain's USDC, a direct delivery to the user or an arrival into the Runner
/// through the pinned handler with a message rebuilt byte for byte, a fill window and a short
/// exclusivity, a fresh quote, and a relayer's fee between the bounds.
fn across_delivery(
    deposit: &AcrossDeposit,
    order: &Order,
    source: &EvmChain,
    chains: &Chains,
) -> Result<(Delivery, Vec<Party>), RelayError> {
    let rule = RelayError::Across;
    let fee_bounds = chains.across_fee.ok_or(rule("across_fee"))?;
    // AX5: the refund and every speed-up are the payer's
    if deposit.depositor != swapic::evm(order.payer) {
        return Err(rule("AX5"));
    }
    // AX6: another chain the canister holds Across's pins for, the source's USDC in, its USDC out
    let destination_id = ChainId::new(deposit.destination_chain_id);
    let destination = chains
        .evm_chains
        .get(destination_id)
        .filter(|_| destination_id != chains.chain_id)
        .ok_or(rule("AX6"))?;
    let pins = destination.across.ok_or(rule("AX6"))?;
    if deposit.input_token != source.usdc || deposit.output_token != destination.usdc {
        return Err(rule("AX6"));
    }
    let ours = |address: EvmAddress| {
        address == EvmAddress::ZERO
            || address == chains.canister
            || [destination.swapic, destination.runner, destination.cctp_receiver].contains(&address)
            || [pins.periphery, pins.spoke_pool, pins.multicall_handler, pins.swap_proxy].contains(&address)
    };
    let deadline = u64::try_from(order.deadline).unwrap_or(u64::MAX);
    let fill = u64::from(deposit.fill_deadline);
    // AX7 and AM1 to AM4: direct to the user, or into the Runner
    let (delivery, recipient) = if deposit.message.is_empty() {
        if ours(deposit.recipient) {
            return Err(rule("AX7"));
        }
        let delivery = Delivery {
            chain: destination_id,
            recipient: deposit.recipient,
            token: deposit.output_token,
            min_out: TokenAmount::from_u256(deposit.output_amount),
        };
        (delivery, deposit.recipient)
    } else {
        if deposit.recipient != pins.multicall_handler {
            return Err(rule("AX7"));
        }
        let (envelope, recipe) = across::read_arrival(
            &deposit.message,
            pins.multicall_handler,
            destination.usdc,
            destination.runner,
        )
        .map_err(|_| rule("AM1"))?;
        if ours(envelope.recipient) {
            return Err(rule("AM2"));
        }
        let sealed = envelope.action == Action::Swap
            && envelope.reference == order.r#ref.0
            && envelope.deadline == fill
            && envelope.token_out != EvmAddress::ZERO
            && envelope.token_out != deposit.output_token
            && !envelope.min_out.is_zero()
            && envelope.min_gas > 0
            && envelope.min_gas <= MAX_MIN_GAS
            && envelope.principal == [0; 32]
            && envelope.subaccount == [0; 32];
        if !sealed {
            return Err(rule("AM3"));
        }
        // AM4: the recipe is KyberSwap's, read as a swap step of the destination's USDC there
        if swapic::evm(recipe.target) != KYBER_ROUTER || swapic::evm(recipe.scaler) != swapic::KYBER_SCALER {
            return Err(rule("AM4"));
        }
        let as_step = Step {
            target: recipe.target,
            data: recipe.data.clone(),
            token: swapic::addr(destination.usdc),
            min: U256::ZERO,
            writes: Vec::new(),
        };
        match swapic::read_step(&as_step, destination.usdc, destination.token_messenger) {
            Ok(KnownStep::Swap(swap)) if swap.dst_token == envelope.token_out => {
                check_swap(&swap, &as_step, &destination, chains.canister).map_err(|_| rule("AM4"))?
            }
            _ => return Err(rule("AM4")),
        }
        let delivery = Delivery {
            chain: destination_id,
            recipient: envelope.recipient,
            token: envelope.token_out,
            min_out: TokenAmount::from_u256(envelope.min_out),
        };
        (delivery, envelope.recipient)
    };
    // AX8: the fill window after the order's deadline
    if fill < deadline.saturating_add(FILL_AFTER_LEAST_S) || fill > deadline.saturating_add(FILL_AFTER_MOST_S) {
        return Err(rule("AX8"));
    }
    // AX9: no exclusivity, or a short one
    let exclusive = match (deposit.exclusive_relayer, deposit.exclusivity) {
        (EvmAddress::ZERO, 0) => true,
        (EvmAddress::ZERO, _) => false,
        (_, offset) => offset > 0 && offset <= MAX_EXCLUSIVITY_S,
    };
    if !exclusive {
        return Err(rule("AX9"));
    }
    // AX12: a quote time the SpokePool takes
    if deadline > u64::from(deposit.quote_timestamp).saturating_add(QUOTE_LIFE_S) {
        return Err(rule("AX12"));
    }
    // AX13 and AX14: F between the least and the most a relayer keeps, in micro USD; never
    // nothing, which a small deposit's gas allowance alone would let through
    let million = U256::from(1_000_000u64);
    let paid = destination.usdc_to_usd6(TokenAmount::from_u256(deposit.output_amount)).to_u256() * million;
    if paid.is_zero() {
        return Err(rule("AX14"));
    }
    let put = source.usdc_to_usd6(TokenAmount::from_u256(deposit.min_expected)).to_u256();
    if paid > put * (million - U256::from(fee_bounds.least_ppm)) {
        return Err(rule("AX13"));
    }
    let allowance = fee_bounds
        .gas_allowance
        .get(destination_id)
        .map(|usd6| usd6.to_u256())
        .unwrap_or(U256::ZERO);
    let floor = (put * (million - U256::from(fee_bounds.most_ppm))).saturating_sub(allowance * million);
    if paid < floor {
        return Err(rule("AX14"));
    }
    Ok((delivery, vec![Party::Evm(recipient)]))
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
    // the memo rides in the calldata the canister pays for, and no deposit's is longer
    if order.memo.len() > types::pool::memo::MAX_MEMO_BYTES {
        return Err(RelayError::Order(
            "carries a memo longer than any deposit's terms",
        ));
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
    // the fee in micro USD, as every row counts it: a chain whose USDC has more decimals
    // (BNB Chain's 18) would otherwise pass a fee 10^12 times too small (BSC design D5)
    let fee = match plan.fees.as_slice() {
        [] => return Err(RelayError::NoFeeStep),
        [fee] => source.usdc_to_usd6(TokenAmount::from_u256(*fee)),
        more => return Err(RelayError::FeeSteps(more.len())),
    };
    let Plan {
        action,
        arrival,
        parties,
        ..
    } = plan;

    // a relay's actions always have their rows: an absent one asks the most
    let mut needed = source
        .min_network_fee
        .get(action)
        .unwrap_or(TokenAmount::MAX);
    if let Some(arrival) = &arrival {
        let destination = chains
            .evm_chains
            .get(arrival.chain_id)
            .expect("BUG: arrival_of found the destination in this table");
        needed = needed
            .checked_add(
                destination
                    .min_network_fee
                    .get(arrival.action)
                    .unwrap_or(TokenAmount::MAX),
            )
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

/// What `relay_gasless`, `finish_cctp` and `pay_payout` do with the reading the watcher
/// priced them on, right after their guards and before anything else: it is stored, stamped
/// `at`, so the fee rule reads it fresh. None leaves the stored reading as it is. An invalid
/// one refuses the call, and nothing is stored or signed. The reading names no chain: it is
/// the call's.
///
/// The newest arrival wins, whatever block it names: the canister's own reads and the
/// watcher's come from different providers, and a stored reading that aged out must never
/// keep out the one a call brings. The bounds every price is held to (`tx::fees`) are what
/// an old or a wrong reading meets.
pub fn take_reading(
    chain_id: ChainId,
    reading: Option<ChainData>,
    at: Timestamp,
) -> Result<(), String> {
    let Some(reading) = reading else {
        return Ok(());
    };
    let reading = types::ChainReading::try_from(reading).map_err(|e| {
        format!(
            "InvalidData({:?})",
            settlement_api::types::chain_data::ChainDataError::from(e)
        )
    })?;
    chain_data::put(chain_id, reading, at);
    Ok(())
}

/// `relay_gasless`: answers the signed transaction's id in `signed_txs`. Its status is
/// `relay_status(order_id)`. `reading` is the source chain's, the one the watcher priced
/// the relay on.
pub async fn relay_gasless(
    chain_id: ChainId,
    order_bytes: &[u8],
    auth_bytes: &[u8],
    gas_limit: u64,
    reading: Option<ChainData>,
) -> Result<u64, String> {
    // the role and the halt before anything else, and before any outcall
    require_watcher().map_err(|e| format!("{e:?}"))?;
    require_not_halted().map_err(|e| format!("{e:?}"))?;
    // the reading the watcher priced this call on: stored first, so the fee rule reads it
    // fresh; with none (or one too old) the canister reads the chain's gas itself, before
    // any check below
    take_reading(
        chain_id,
        reading,
        Timestamp::from_nanos(ic_cdk::api::time()),
    )?;
    crate::gas::ensure_fresh(chain_id)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))?;
    let order = swapic::decode_order(order_bytes).ok_or(RelayError::NotAnOrder.to_string())?;
    let auth = swapic::decode_auth(auth_bytes).ok_or(RelayError::NotAnAuth.to_string())?;
    let config = config::get();
    // an order already relayed answers its relay, whatever came after it: the clock, the
    // config, a listing. Its id is the one every door recomputes, on this chain's Swapic
    let source = config
        .evm_chains
        .get(chain_id)
        .ok_or(RelayError::UnknownChain(chain_id).to_string())?;
    let key = QuoteHash::new(swapic::order_id(&order, chain_id, source.swapic).into_bytes());
    if let Some(id) = tx::signed_once(key)? {
        return Ok(id);
    }
    // derived once by the deploy and read from stable memory after that
    let canister = ecdsa::canister_address().await.map_err(|e| e.to_string())?;
    let settings = flow_settings::get();
    let now_s = Timestamp::from_nanos(ic_cdk::api::time()).as_secs().get();
    let chains = Chains {
        chain_id,
        evm_chains: &config.evm_chains,
        domains: &config.cctp_domains,
        canister,
        eco_portal: config.eco_portal,
        across_fee: config.across_fee.as_ref(),
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
    authorize(&order, &auth, relay.order_id, chain_id, &source).map_err(|e| e.to_string())?;
    if let Some(party) = sanctioned_party(&relay.parties) {
        return Err(RelayError::Sanctioned(party).to_string());
    }

    let _busy = Busy::take(key).ok_or("the order is being signed")?;
    let swapic_address = config
        .evm_chains
        .get(chain_id)
        .expect("BUG: check found the chain in this table")
        .swapic;
    let mut data = swapic::run_for_calldata(&order, &auth);
    // an Across order names Swapic's integrator after its call (BSC design 6.2.11): Solidity
    // ignores trailing calldata and the order id does not cover it
    if relay.action == GasAction::RelayAcross {
        if let Some(id) = config.across_integrator {
            data.extend_from_slice(&across::integrator_suffix(id));
        }
    }
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
    // a reading that aged past its limit while the simulation was out is read again, not
    // refused; the halt and a concurrent relay are checked again after that read
    if crate::gas::refresh_if_aged(chain_id)
        .await
        .map_err(|failure| format!("the gas price: {failure:?}"))?
    {
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
