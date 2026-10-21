//! Swapic ABI v3 (design C) as this canister speaks to it: the order a user signs and its
//! EIP-712 fingerprint, the gasless authorization, the arrival envelope and recipe, the
//! steps the canister reads (its fee, a CCTP burn), and the calls it signs into Swapic,
//! the CctpReceiver and CCTP's `MessageTransmitterV2`.
//!
//! The byte-level contract is P9c's `abi.md`, and its goldens are the tests. Ported from
//! the services' own ABI v3 module: this crate is public and the services are not, so the
//! port is a copy and never a dependency.
//!
//! On the wire an order and an authorization are each `abi.encode` of ONE dynamic tuple,
//! with the leading offset word (`0x20`): what Solidity's `abi.encode(order)` and viem's
//! `encodeAbiParameters([OrderTuple], [order])` write, and what `runFor` carries after its
//! selector.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::evm::EvmAddress;
use alloy_primitives::{keccak256, Address, FixedBytes, U256};
use alloy_sol_types::abi::AbiDecoderConfig;
use alloy_sol_types::{sol, Eip712Domain, SolCall, SolStruct, SolValue};
use std::borrow::Cow;
use std::fmt;
use thiserror::Error;

sol! {
    #![sol(all_derives)]

    /// Writes `(delta - minus)` as one word at byte `at` of a step's data.
    struct Write { uint32 at; uint256 minus; }

    /// One call the Runner makes: `token` ERC-20 is approved exactly its delta, `NATIVE`
    /// sends it as value, the zero address makes a plain call with no writes.
    struct Step { address target; bytes data; address token; uint256 min; Write[] writes; }

    /// `token == 0`: no payout (a cross-chain order).
    struct Payout { address token; address recipient; uint256 minOut; }

    struct Order {
        bytes32 ref;
        address payer;
        address tokenIn;
        uint256 amountIn;
        uint256 deadline;
        Step[] steps;
        Payout payout;
    }

    /// `door` is the `Door` enum as the ABI carries it.
    struct GaslessAuth {
        uint8 door;
        uint256 validAfter;
        uint256 validBefore;
        uint256 nonce;
        uint256 deadline;
        bytes signature;
        bytes acceptance;
    }

    /// The arrival swap; `scaler == 0` runs `data` as it is.
    struct Recipe { address target; bytes data; address scaler; }

    function runFor(Order order, GaslessAuth auth) external returns (bytes32 id);
    function completeCctp(bytes message, bytes attestation, Recipe recipe) external;
    function release(bytes message, bytes attestation) external;
    function receiveMessage(bytes message, bytes attestation) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes hookData
    ) external;

    event OrderStarted(bytes32 indexed id, bytes32 indexed ref, address indexed payer, address tokenIn, uint256 amountIn);
    event Transfer(address indexed from, address indexed to, uint256 value);
    /// The Runner paid a same-chain order's payout.
    event PaidOut(bytes32 indexed id, address indexed recipient, address token, uint256 amount);
    /// CCTP v2's `MessageTransmitterV2` received a message.
    event MessageReceived(address indexed caller, uint32 sourceDomain, bytes32 indexed nonce, bytes32 sender, uint32 indexed finalityThresholdExecuted, bytes messageBody);
    /// Eco Portal 2.12's intent events (the services' `onchain::eco`, read off its bytecode).
    event IntentPublished(bytes32 indexed intentHash, uint64 destination, bytes route, address indexed creator, address indexed prover, uint64 rewardDeadline, uint256 rewardNativeAmount, EcoTokenAmount[] rewardTokens);
    event IntentFulfilled(bytes32 indexed intentHash, bytes32 indexed claimant);
    /// Eco Portal 2.12's refund of an unfilled intent's reward to its creator (the services'
    /// `onchain::eco`, read off its bytecode): the hash in the data, the refundee a topic.
    event IntentRefunded(bytes32 intentHash, address indexed refundee);

    /// Eco Portal 2.12's intent, as the quoter writes it into a `publishAndFund` step.
    struct EcoCall { address target; bytes data; uint256 value; }
    struct EcoTokenAmount { address token; uint256 amount; }
    struct EcoRoute {
        bytes32 salt;
        uint64 deadline;
        address portal;
        uint256 nativeAmount;
        EcoTokenAmount[] tokens;
        EcoCall[] calls;
    }
    struct EcoReward {
        uint64 deadline;
        address creator;
        address prover;
        uint256 nativeAmount;
        EcoTokenAmount[] tokens;
    }
    function publishAndFund(uint64 destination, bytes route, EcoReward reward, bool allowPartial) external payable returns (bytes32 intentHash, address vault);
    function approve(address spender, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function run(Order order) external payable returns (bytes32 id);
    function used(bytes32 id) external view returns (bool);
    function fill(address token, uint256 amount, bytes envelope, Recipe recipe) external;

    /// KyberSwap's MetaAggregationRouterV2 `swap`, the swap step the quoter writes.
    struct KyberDescription {
        address srcToken;
        address dstToken;
        address[] srcReceivers;
        uint256[] srcAmounts;
        address[] feeReceivers;
        uint256[] feeAmounts;
        address dstReceiver;
        uint256 amount;
        uint256 minReturnAmount;
        uint256 flags;
        bytes permit;
    }
    struct KyberExecution {
        address callTarget;
        address approveTarget;
        bytes targetData;
        KyberDescription desc;
        bytes clientData;
    }
    function swap(KyberExecution execution) external payable returns (uint256 returnAmount, uint256 gasUsed);

    /// USDC's EIP-3009 pull, as the payer signs it for the 3009 door: the nonce is the
    /// order id.
    struct ReceiveWithAuthorization {
        address from;
        address to;
        uint256 value;
        uint256 validAfter;
        uint256 validBefore;
        bytes32 nonce;
    }

    /// Permit2's witness transfer, as the payer signs it for the Permit2 door: the witness
    /// is the order id, the spender Swapic.
    struct TokenPermissions { address token; uint256 amount; }
    struct OrderWitness { bytes32 orderId; }
    struct PermitWitnessTransferFrom {
        TokenPermissions permitted;
        address spender;
        uint256 nonce;
        uint256 deadline;
        OrderWitness witness;
    }
}

/// An Eco intent as the source chain's `IntentPublished` log says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedIntent {
    pub intent_hash: [u8; 32],
    pub destination: u64,
    pub creator: EvmAddress,
    /// `abi.encode` of the route, its amounts as the Runner wrote them.
    pub route: Vec<u8>,
}

/// The intent an `IntentPublished` log publishes, None for a log that is not one.
pub fn read_intent_published(topics: &[[u8; 32]], data: &[u8]) -> Option<PublishedIntent> {
    use alloy_sol_types::SolEvent;
    let event = IntentPublished::decode_raw_log_with_config(
        topics.iter().map(|topic| FixedBytes(*topic)),
        data,
        strict(),
    )
    .ok()?;
    Some(PublishedIntent {
        intent_hash: event.intentHash.0,
        destination: event.destination,
        creator: evm(event.creator),
        route: event.route.to_vec(),
    })
}

/// A message CCTP v2's transmitter received, as its `MessageReceived` log says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedMessage {
    /// The CCTP domain the message left from.
    pub source_domain: u32,
    /// The message's sender on that domain: its `TokenMessengerV2` for a burn.
    pub sender: [u8; 32],
    /// The burn body, its hook data at its end.
    pub body: Vec<u8>,
}

/// The message a `MessageReceived` log carries, None for a log that is not one.
pub fn read_message_received(topics: &[[u8; 32]], data: &[u8]) -> Option<ReceivedMessage> {
    use alloy_sol_types::SolEvent;
    let event = MessageReceived::decode_raw_log_with_config(
        topics.iter().map(|topic| FixedBytes(*topic)),
        data,
        strict(),
    )
    .ok()?;
    Some(ReceivedMessage {
        source_domain: event.sourceDomain,
        sender: event.sender.0,
        body: event.messageBody.to_vec(),
    })
}

/// The one delivery an Eco Deliver route makes, `token.transfer(to, amount)`: the token, the
/// recipient and the amount. None for every other route (a fill, more than one call or
/// token, any ether).
pub fn route_delivery(route: &[u8]) -> Option<(EvmAddress, EvmAddress, U256)> {
    let route = <EcoRoute as SolValue>::abi_decode_with_config(route, strict()).ok()?;
    let [carried] = route.tokens.as_slice() else {
        return None;
    };
    let [send] = route.calls.as_slice() else {
        return None;
    };
    if send.target != carried.token || !send.value.is_zero() || !route.nativeAmount.is_zero() {
        return None;
    }
    let call = transferCall::abi_decode_with_config(&send.data, strict()).ok()?;
    Some((evm(carried.token), evm(call.to), call.amount))
}

/// A Deliver route's bytes: `token.transfer(to, amount)` and nothing else, as the quoter
/// writes a pool deposit's Eco route.
pub fn deliver_route(
    salt: [u8; 32],
    deadline: u64,
    portal: EvmAddress,
    token: EvmAddress,
    to: EvmAddress,
    amount: U256,
) -> Vec<u8> {
    SolValue::abi_encode(&EcoRoute {
        salt: FixedBytes(salt),
        deadline,
        portal: addr(portal),
        nativeAmount: U256::ZERO,
        tokens: vec![EcoTokenAmount {
            token: addr(token),
            amount,
        }],
        calls: vec![EcoCall {
            target: addr(token),
            data: transferCall {
                to: addr(to),
                amount,
            }
            .abi_encode()
            .into(),
            value: U256::ZERO,
        }],
    })
}

/// The topic0 of the events a pool arrival is proven by.
pub mod topics {
    use super::*;
    use alloy_sol_types::SolEvent;

    pub fn order_started() -> [u8; 32] {
        OrderStarted::SIGNATURE_HASH.0
    }

    pub fn transfer() -> [u8; 32] {
        Transfer::SIGNATURE_HASH.0
    }

    pub fn paid_out() -> [u8; 32] {
        PaidOut::SIGNATURE_HASH.0
    }

    pub fn message_received() -> [u8; 32] {
        MessageReceived::SIGNATURE_HASH.0
    }

    pub fn intent_published() -> [u8; 32] {
        IntentPublished::SIGNATURE_HASH.0
    }

    pub fn intent_fulfilled() -> [u8; 32] {
        IntentFulfilled::SIGNATURE_HASH.0
    }

    pub fn intent_refunded() -> [u8; 32] {
        IntentRefunded::SIGNATURE_HASH.0
    }
}

/// Uniswap's Permit2, one address on every chain: the Permit2 door's verifier.
pub const PERMIT2: EvmAddress = EvmAddress::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x22, 0xd4, 0x73, 0x03, 0x0f, 0x11, 0x6d, 0xde, 0xe9, 0xf6, 0xb4,
    0x3a, 0xc7, 0x8b, 0xa3,
]);

/// Circle's USDC EIP-712 domain, the same name and version on every chain the quoter
/// routes on: what the 3009 door's authorization is signed under.
pub const USDC_NAME: &str = "USD Coin";
pub const USDC_VERSION: &str = "2";

/// The digest a 3009 door's authorization signs: USDC's `ReceiveWithAuthorization` from
/// the payer to Swapic of `amountIn`, its nonce the order id, under the chain's USDC's own
/// domain. What `receiveWithAuthorization` recovers on chain.
pub fn receive_with_authorization_digest(
    order: &Order,
    auth: &GaslessAuth,
    id: OrderId,
    chain_id: ChainId,
    usdc: EvmAddress,
    swapic: EvmAddress,
) -> [u8; 32] {
    let domain = Eip712Domain {
        name: Some(Cow::Borrowed(USDC_NAME)),
        version: Some(Cow::Borrowed(USDC_VERSION)),
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(usdc)),
        salt: None,
    };
    ReceiveWithAuthorization {
        from: order.payer,
        to: addr(swapic),
        value: order.amountIn,
        validAfter: auth.validAfter,
        validBefore: auth.validBefore,
        nonce: FixedBytes(id.into_bytes()),
    }
    .eip712_signing_hash(&domain)
    .0
}

/// The digest a Permit2 door's authorization signs: Permit2's `PermitWitnessTransferFrom`
/// of `amountIn` of `tokenIn`, Swapic the spender, the order id the witness, under
/// Permit2's domain (no version). What `permitWitnessTransferFrom` recovers on chain.
pub fn permit2_digest(
    order: &Order,
    auth: &GaslessAuth,
    id: OrderId,
    chain_id: ChainId,
    swapic: EvmAddress,
) -> [u8; 32] {
    let domain = Eip712Domain {
        name: Some(Cow::Borrowed("Permit2")),
        version: None,
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(PERMIT2)),
        salt: None,
    };
    PermitWitnessTransferFrom {
        permitted: TokenPermissions {
            token: order.tokenIn,
            amount: order.amountIn,
        },
        spender: addr(swapic),
        nonce: auth.nonce,
        deadline: auth.deadline,
        witness: OrderWitness {
            orderId: FixedBytes(id.into_bytes()),
        },
    }
    .eip712_signing_hash(&domain)
    .0
}

/// How every ABI value this canister reads is decoded: strictly. Canonical offsets, zero
/// padding, every narrow word in range and no trailing bytes, so what the canister reads
/// is what the contract it calls reads, and a lenient encoding is refused rather than read
/// differently.
fn strict() -> AbiDecoderConfig {
    AbiDecoderConfig::new().strict(true)
}

/// Ether wherever a token address is expected.
pub const NATIVE: EvmAddress = EvmAddress::new([0xee; 20]);

/// Eco Portal's `publishAndFund(uint64,bytes,Reward,bool)`: the step that puts an order on
/// the Eco rail.
pub const PUBLISH_AND_FUND: [u8; 4] = [0xdf, 0x00, 0xf8, 0xfa];

/// The EIP-712 domain's name and version.
pub const NAME: &str = "Swapic";
pub const VERSION: &str = "3";

/// The encoded types `hashStruct` is over. Spelled out rather than taken from the `sol!`
/// structs: `ref` is a Rust keyword, and the macro writes it into the type string as
/// `r#ref`, which hashes another type.
pub const ORDER_TYPE: &str = "Order(bytes32 ref,address payer,address tokenIn,uint256 amountIn,\
uint256 deadline,Step[] steps,Payout payout)Payout(address token,address recipient,uint256 minOut)\
Step(address target,bytes data,address token,uint256 min,Write[] writes)Write(uint32 at,uint256 minus)";
pub const STEP_TYPE: &str = "Step(address target,bytes data,address token,uint256 min,\
Write[] writes)Write(uint32 at,uint256 minus)";
pub const WRITE_TYPE: &str = "Write(uint32 at,uint256 minus)";
pub const PAYOUT_TYPE: &str = "Payout(address token,address recipient,uint256 minOut)";
pub const ACCEPTANCE_TYPE: &str =
    "QuoteAcceptance(bytes32 orderId,address payer,address tokenIn,uint256 amountIn,uint256 deadline)";
/// Permit2's witness: the order id under this type.
pub const WITNESS_TYPE: &str = "OrderWitness(bytes32 orderId)";

/// `Door` on the wire.
pub const DOOR_AUTH_3009: u8 = 0;
pub const DOOR_PERMIT_2612: u8 = 1;
pub const DOOR_PERMIT2: u8 = 2;

/// The fingerprint of one order on one chain's Swapic: the 3009 nonce, the Permit2
/// witness, the QuoteAcceptance's subject, and the key a relay is held once under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderId([u8; 32]);

impl OrderId {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    /// The id as the 3009 door's nonce, a uint256.
    pub fn as_nonce(self) -> U256 {
        U256::from_be_bytes(self.0)
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

/// Swapic's EIP-712 domain on `chain_id` at `swapic`.
pub fn domain(chain_id: ChainId, swapic: EvmAddress) -> Eip712Domain {
    Eip712Domain {
        name: Some(Cow::Borrowed(NAME)),
        version: Some(Cow::Borrowed(VERSION)),
        chain_id: Some(U256::from(chain_id.get())),
        verifying_contract: Some(addr(swapic)),
        salt: None,
    }
}

fn word(value: U256) -> [u8; 32] {
    value.to_be_bytes::<32>()
}

/// `hashStruct(Write)`.
pub fn hash_write(w: &Write) -> [u8; 32] {
    let mut encoded = Vec::with_capacity(32 * 3);
    encoded.extend_from_slice(keccak256(WRITE_TYPE).as_slice());
    encoded.extend_from_slice(&word(U256::from(w.at)));
    encoded.extend_from_slice(&word(w.minus));
    keccak256(encoded).0
}

/// `hashStruct(Step)`: `bytes` hashed, the writes as keccak of their hashes concatenated.
pub fn hash_step(s: &Step) -> [u8; 32] {
    let writes: Vec<u8> = s.writes.iter().flat_map(hash_write).collect();
    let mut encoded = Vec::with_capacity(32 * 6);
    encoded.extend_from_slice(keccak256(STEP_TYPE).as_slice());
    encoded.extend_from_slice(s.target.into_word().as_slice());
    encoded.extend_from_slice(keccak256(&s.data).as_slice());
    encoded.extend_from_slice(s.token.into_word().as_slice());
    encoded.extend_from_slice(&word(s.min));
    encoded.extend_from_slice(keccak256(writes).as_slice());
    keccak256(encoded).0
}

/// `hashStruct(Order)`, as `OrderHash.hashOrder` computes it.
pub fn hash_order(o: &Order) -> [u8; 32] {
    let steps: Vec<u8> = o.steps.iter().flat_map(hash_step).collect();
    let mut encoded = Vec::with_capacity(32 * 8);
    encoded.extend_from_slice(keccak256(ORDER_TYPE).as_slice());
    encoded.extend_from_slice(o.r#ref.as_slice());
    encoded.extend_from_slice(o.payer.into_word().as_slice());
    encoded.extend_from_slice(o.tokenIn.into_word().as_slice());
    encoded.extend_from_slice(&word(o.amountIn));
    encoded.extend_from_slice(&word(o.deadline));
    encoded.extend_from_slice(keccak256(steps).as_slice());
    // `Payout` has no keyword in it, so the macro's own type string is the contract's
    encoded.extend_from_slice(o.payout.eip712_hash_struct().as_slice());
    keccak256(encoded).0
}

/// `keccak256(0x1901 ++ separator ++ structHash)`.
pub fn digest(separator: [u8; 32], struct_hash: [u8; 32]) -> [u8; 32] {
    let mut packed = Vec::with_capacity(66);
    packed.extend_from_slice(&[0x19, 0x01]);
    packed.extend_from_slice(&separator);
    packed.extend_from_slice(&struct_hash);
    keccak256(packed).0
}

/// THE ORDER ID of `order` for `swapic` on `chain_id`, as every door recomputes it.
pub fn order_id(order: &Order, chain_id: ChainId, swapic: EvmAddress) -> OrderId {
    OrderId(digest(
        domain(chain_id, swapic).separator().0,
        hash_order(order),
    ))
}

/// What the payer signs on the 2612 door: the QuoteAcceptance of the order's id.
pub fn acceptance_digest(order: &Order, chain_id: ChainId, swapic: EvmAddress) -> [u8; 32] {
    let id = order_id(order, chain_id, swapic);
    let mut encoded = Vec::with_capacity(32 * 6);
    encoded.extend_from_slice(keccak256(ACCEPTANCE_TYPE).as_slice());
    encoded.extend_from_slice(&id.0);
    encoded.extend_from_slice(order.payer.into_word().as_slice());
    encoded.extend_from_slice(order.tokenIn.into_word().as_slice());
    encoded.extend_from_slice(&word(order.amountIn));
    encoded.extend_from_slice(&word(order.deadline));
    digest(domain(chain_id, swapic).separator().0, keccak256(encoded).0)
}

/// The order an `abi.encode(order)` holds, encoded canonically.
pub fn decode_order(bytes: &[u8]) -> Option<Order> {
    <Order as SolValue>::abi_decode_with_config(bytes, strict()).ok()
}

/// `abi.encode(order)`: the order exactly as signed.
pub fn encode_order(order: &Order) -> Vec<u8> {
    SolValue::abi_encode(order)
}

/// The authorization an `abi.encode(auth)` holds, encoded canonically.
pub fn decode_auth(bytes: &[u8]) -> Option<GaslessAuth> {
    <GaslessAuth as SolValue>::abi_decode_with_config(bytes, strict()).ok()
}

/// `abi.encode(auth)`.
pub fn encode_auth(auth: &GaslessAuth) -> Vec<u8> {
    SolValue::abi_encode(auth)
}

/// The recipe an `abi.encode(recipe)` holds.
pub fn decode_recipe(bytes: &[u8]) -> Option<Recipe> {
    <Recipe as SolValue>::abi_decode(bytes).ok()
}

/// `Swapic.run(order)`: the canister's own pool payout, paid from its own USDC.
pub fn run_calldata(order: &Order) -> Vec<u8> {
    runCall {
        order: order.clone(),
    }
    .abi_encode()
}

/// `token.approve(spender, amount)`.
pub fn approve_calldata(spender: EvmAddress, amount: U256) -> Vec<u8> {
    approveCall {
        spender: addr(spender),
        amount,
    }
    .abi_encode()
}

/// `token.transfer(to, amount)`.
pub fn transfer_calldata(to: EvmAddress, amount: U256) -> Vec<u8> {
    transferCall {
        to: addr(to),
        amount,
    }
    .abi_encode()
}

/// `Swapic.used(id)`: whether an order id ran.
pub fn used_calldata(id: [u8; 32]) -> Vec<u8> {
    usedCall { id: FixedBytes(id) }.abi_encode()
}

/// The refunded intent's hash and its refundee, from an `IntentRefunded` log; None for a log
/// that is not one.
pub fn read_intent_refunded(topics: &[[u8; 32]], data: &[u8]) -> Option<([u8; 32], EvmAddress)> {
    use alloy_sol_types::SolEvent;
    let event = IntentRefunded::decode_raw_log_with_config(
        topics.iter().map(|topic| FixedBytes(*topic)),
        data,
        strict(),
    )
    .ok()?;
    Some((event.intentHash.0, evm(event.refundee)))
}

/// `token.allowance(owner, spender)`.
pub fn allowance_calldata(owner: EvmAddress, spender: EvmAddress) -> Vec<u8> {
    allowanceCall {
        owner: addr(owner),
        spender: addr(spender),
    }
    .abi_encode()
}

/// `Swapic.runFor(order, auth)`: the canister's relay of a gasless order.
pub fn run_for_calldata(order: &Order, auth: &GaslessAuth) -> Vec<u8> {
    runForCall {
        order: order.clone(),
        auth: auth.clone(),
    }
    .abi_encode()
}

/// `CctpReceiver.completeCctp(message, attestation, recipe)`: a Swap or a CkDeposit
/// arrival, before its envelope's deadline.
pub fn complete_cctp_calldata(message: &[u8], attestation: &[u8], recipe: Recipe) -> Vec<u8> {
    completeCctpCall {
        message: message.to_vec().into(),
        attestation: attestation.to_vec().into(),
        recipe,
    }
    .abi_encode()
}

/// `CctpReceiver.release(message, attestation)`: the minted USDC to the envelope's
/// recipient, after the deadline.
pub fn release_calldata(message: &[u8], attestation: &[u8]) -> Vec<u8> {
    releaseCall {
        message: message.to_vec().into(),
        attestation: attestation.to_vec().into(),
    }
    .abi_encode()
}

/// `MessageTransmitterV2.receiveMessage(message, attestation)`: a Deliver, minted straight
/// to the user.
pub fn receive_message_calldata(message: &[u8], attestation: &[u8]) -> Vec<u8> {
    receiveMessageCall {
        message: message.to_vec().into(),
        attestation: attestation.to_vec().into(),
    }
    .abi_encode()
}

/// No recipe: what a CkDeposit completion carries.
pub fn no_recipe() -> Recipe {
    Recipe {
        target: Address::ZERO,
        data: Default::default(),
        scaler: Address::ZERO,
    }
}

/// The selectors of the calls this canister signs or reads, as abi.md section 8 lists
/// them.
pub mod selectors {
    use super::*;

    pub const RUN_FOR: [u8; 4] = runForCall::SELECTOR;
    pub const COMPLETE_CCTP: [u8; 4] = completeCctpCall::SELECTOR;
    pub const RELEASE: [u8; 4] = releaseCall::SELECTOR;
    pub const RECEIVE_MESSAGE: [u8; 4] = receiveMessageCall::SELECTOR;
    pub const TRANSFER: [u8; 4] = transferCall::SELECTOR;
    pub const DEPOSIT_FOR_BURN_WITH_HOOK: [u8; 4] = depositForBurnWithHookCall::SELECTOR;
}

/// The step's selector, if its data has one.
pub fn selector_of(step: &Step) -> Option<[u8; 4]> {
    step.data.get(..4)?.try_into().ok()
}

/// abi.md 6.1, read back: `usdc.transfer(to, amount)`, a fixed amount, no token, no
/// writes. The `(to, amount)` a step pays, if it is exactly that shape on `usdc`.
pub fn fee_transfer_of(step: &Step, usdc: EvmAddress) -> Option<(EvmAddress, U256)> {
    if step.target != addr(usdc)
        || step.token != Address::ZERO
        || !step.writes.is_empty()
        || step.data.len() != 4 + 32 * 2
        || selector_of(step) != Some(selectors::TRANSFER)
    {
        return None;
    }
    let call = transferCall::abi_decode_with_config(&step.data, strict()).ok()?;
    Some((evm(call.to), call.amount))
}

/// A CCTP v2 burn an order makes (abi.md 6.2), read back from its step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CctpBurn {
    pub destination_domain: u32,
    pub mint_recipient: [u8; 32],
    pub burn_token: EvmAddress,
    pub destination_caller: [u8; 32],
    /// The envelope, as the burn carries it to the destination.
    pub hook_data: Vec<u8>,
    /// The most Circle may keep of the burn.
    pub max_fee: U256,
}

impl CctpBurn {
    /// The burn a step to `messenger` makes: `None` for a step to anything else, and an
    /// error for a step to the messenger that is not `depositForBurnWithHook`.
    pub fn of_step(step: &Step, messenger: EvmAddress) -> Option<Result<Self, NotABurn>> {
        if step.target != addr(messenger) {
            return None;
        }
        if selector_of(step) != Some(selectors::DEPOSIT_FOR_BURN_WITH_HOOK) {
            return Some(Err(NotABurn));
        }
        Some(
            depositForBurnWithHookCall::abi_decode_with_config(&step.data, strict())
                .map(|call| Self {
                    destination_domain: call.destinationDomain,
                    mint_recipient: call.mintRecipient.0,
                    burn_token: evm(call.burnToken),
                    destination_caller: call.destinationCaller.0,
                    hook_data: call.hookData.to_vec(),
                    max_fee: call.maxFee,
                })
                .map_err(|_| NotABurn),
        )
    }
}

/// A step to CCTP's token messenger that is not a `depositForBurnWithHook` this canister
/// reads.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a step to the token messenger is not a depositForBurnWithHook")]
pub struct NotABurn;

/// An Eco intent an order publishes (abi.md 6.3), read back from its step: who created it,
/// and where its route delivers on the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcoIntent {
    /// The destination chain id.
    pub destination: u64,
    /// The reward's creator: the user, whom an unfilled intent refunds (rule 19).
    pub creator: EvmAddress,
    /// The one token the route carries, on the destination.
    pub token: EvmAddress,
    /// The one token the reward pays the solver, on the source.
    pub reward_token: EvmAddress,
    pub arrival: EcoArrival,
    /// Who proves the fill to the source chain: its word pays the reward.
    pub prover: EvmAddress,
    /// The destination Portal the route is fulfilled on.
    pub portal: EvmAddress,
    /// The last instant the route may be fulfilled, unix seconds.
    pub route_deadline: u64,
    /// When an unfilled intent's reward may be refunded to its creator, unix seconds.
    pub reward_deadline: u64,
}

/// What an Eco route does on the destination: the two shapes the quoter writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcoArrival {
    /// `[token.transfer(recipient, X - fee)]`.
    Deliver { recipient: EvmAddress },
    /// `[token.approve(runner, X - fee), runner.fill(token, X - fee, envelope, recipe)]`:
    /// a Swap or a CkDeposit.
    Fill {
        runner: EvmAddress,
        envelope: Envelope,
    },
}

/// Why a `publishAndFund` step is not an intent this canister can read.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum NotAnIntent {
    #[error("the step is not a publishAndFund call")]
    Call,
    #[error("the route is not abi.encode of an Eco route")]
    Route,
    #[error("the route carries other than exactly one token")]
    Tokens,
    #[error("the route or the reward moves ether")]
    Value,
    #[error("the reward pays other than exactly one token")]
    RewardTokens,
    #[error("the intent may be funded in part")]
    Partial,
    #[error("the route's calls are neither a transfer nor an approve and a fill")]
    Calls,
    #[error("the fill's envelope: {0}")]
    Envelope(EnvelopeError),
}

impl EcoIntent {
    /// The intent a `publishAndFund` step publishes: `None` for any other step, and an
    /// error for one this canister cannot read.
    pub fn of_step(step: &Step) -> Option<Result<Self, NotAnIntent>> {
        (selector_of(step) == Some(PUBLISH_AND_FUND))
            .then(|| Self::read(&step.data).map(|(intent, _, _)| intent))
    }

    /// The intent `data` publishes, the call rebuilt as the quoter builds it (every amount
    /// zero: the Runner writes them), and the length of the rebuilt route, which places
    /// the reward's write.
    fn read(data: &[u8]) -> Result<(Self, Vec<u8>, usize), NotAnIntent> {
        let call = publishAndFundCall::abi_decode_with_config(data, strict())
            .map_err(|_| NotAnIntent::Call)?;
        let route = <EcoRoute as SolValue>::abi_decode_with_config(&call.route, strict())
            .map_err(|_| NotAnIntent::Route)?;
        if call.allowPartial {
            return Err(NotAnIntent::Partial);
        }
        let [carried] = route.tokens.as_slice() else {
            return Err(NotAnIntent::Tokens);
        };
        let [paid] = call.reward.tokens.as_slice() else {
            return Err(NotAnIntent::RewardTokens);
        };
        if !route.nativeAmount.is_zero()
            || !call.reward.nativeAmount.is_zero()
            || route.calls.iter().any(|call| !call.value.is_zero())
        {
            return Err(NotAnIntent::Value);
        }
        let token = carried.token;
        let selector = |data: &[u8]| -> Option<[u8; 4]> { data.get(..4)?.try_into().ok() };
        let zero = U256::ZERO;
        let at_zero = |target: Address, data: Vec<u8>| EcoCall {
            target,
            data: data.into(),
            value: zero,
        };
        let (arrival, calls) = match route.calls.as_slice() {
            [send] if send.target == token && selector(&send.data) == Some(selectors::TRANSFER) => {
                let send = transferCall::abi_decode_with_config(&send.data, strict())
                    .map_err(|_| NotAnIntent::Calls)?;
                let rebuilt = transferCall {
                    to: send.to,
                    amount: zero,
                }
                .abi_encode();
                (
                    EcoArrival::Deliver {
                        recipient: evm(send.to),
                    },
                    vec![at_zero(token, rebuilt)],
                )
            }
            [grant, fill]
                if grant.target == token
                    && selector(&grant.data) == Some(approveCall::SELECTOR)
                    && selector(&fill.data) == Some(fillCall::SELECTOR) =>
            {
                let approved = approveCall::abi_decode_with_config(&grant.data, strict())
                    .map_err(|_| NotAnIntent::Calls)?;
                let filled = fillCall::abi_decode_with_config(&fill.data, strict())
                    .map_err(|_| NotAnIntent::Calls)?;
                if approved.spender != fill.target || filled.token != token {
                    return Err(NotAnIntent::Calls);
                }
                let envelope = Envelope::decode(&filled.envelope).map_err(NotAnIntent::Envelope)?;
                let grant = approveCall {
                    spender: fill.target,
                    amount: zero,
                }
                .abi_encode();
                let fill_data = fillCall {
                    amount: zero,
                    ..filled
                }
                .abi_encode();
                (
                    EcoArrival::Fill {
                        runner: evm(fill.target),
                        envelope,
                    },
                    vec![at_zero(token, grant), at_zero(fill.target, fill_data)],
                )
            }
            _ => return Err(NotAnIntent::Calls),
        };
        let route_bytes = SolValue::abi_encode(&EcoRoute {
            salt: route.salt,
            deadline: route.deadline,
            portal: route.portal,
            nativeAmount: zero,
            tokens: vec![EcoTokenAmount {
                token,
                amount: zero,
            }],
            calls,
        });
        let route_len = route_bytes.len();
        let intent = Self {
            destination: call.destination,
            creator: evm(call.reward.creator),
            token: evm(token),
            reward_token: evm(paid.token),
            arrival,
            prover: evm(call.reward.prover),
            portal: evm(route.portal),
            route_deadline: route.deadline,
            reward_deadline: call.reward.deadline,
        };
        let rebuilt = publishAndFundCall {
            destination: call.destination,
            route: route_bytes.into(),
            reward: EcoReward {
                deadline: call.reward.deadline,
                creator: call.reward.creator,
                prover: call.reward.prover,
                nativeAmount: zero,
                tokens: vec![EcoTokenAmount {
                    token: paid.token,
                    amount: zero,
                }],
            },
            allowPartial: false,
        }
        .abi_encode();
        Ok((intent, rebuilt, route_len))
    }
}

/// Where the Runner writes the amounts of the shapes the quoter builds, by byte of the
/// step's data (abi.md 6.2 and 6.3).
pub mod offsets {
    /// The burn amount of `depositForBurnWithHook`: the word after the selector.
    pub const CCTP_AMOUNT: u32 = 4;
    /// `publishAndFund(uint64,bytes,Reward,bool)`: the route bytes start after the
    /// selector, the four head words and the bytes' length word.
    pub const ECO_ROUTE_AT: u32 = 0xa4;
    /// Inside the route bytes: the one token's amount.
    pub const ECO_TOKEN_AMOUNT: u32 = 0x120;
    /// Inside the route bytes, Swap and CkDeposit: `approve`'s amount, then `fill`'s.
    pub const ECO_APPROVE_AMOUNT: u32 = 0x244;
    pub const ECO_FILL_AMOUNT: u32 = 0x324;
    /// Inside the route bytes, Deliver: `transfer`'s amount.
    pub const ECO_TRANSFER_AMOUNT: u32 = 0x224;

    /// The reward's one token amount, after the route bytes padded to a word.
    pub fn eco_reward_amount(route_len: usize) -> u32 {
        let padded = route_len.div_ceil(32) * 32;
        ECO_ROUTE_AT + padded as u32 + 0xe0
    }
}

/// The writes of an Eco step (abi.md 6.3): the reward is the whole delta, and the route's
/// token and call amounts are the delta less Eco's `fee`.
pub fn eco_writes(arrival: &EcoArrival, route_len: usize, fee: U256) -> Vec<Write> {
    let write = |at: u32, minus: U256| Write { at, minus };
    let mut writes = vec![
        write(offsets::eco_reward_amount(route_len), U256::ZERO),
        write(offsets::ECO_ROUTE_AT + offsets::ECO_TOKEN_AMOUNT, fee),
    ];
    match arrival {
        EcoArrival::Deliver { .. } => writes.push(write(
            offsets::ECO_ROUTE_AT + offsets::ECO_TRANSFER_AMOUNT,
            fee,
        )),
        EcoArrival::Fill { .. } => writes.extend([
            write(offsets::ECO_ROUTE_AT + offsets::ECO_APPROVE_AMOUNT, fee),
            write(offsets::ECO_ROUTE_AT + offsets::ECO_FILL_AMOUNT, fee),
        ]),
    }
    writes
}

/// A step of a shape this canister reads, read exactly as the Runner will run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KnownStep {
    /// `usdc.transfer(to, amount)`, a fixed amount: the fee step's shape (abi.md 6.1).
    Transfer { to: EvmAddress, amount: U256 },
    /// A CCTP v2 burn of the Runner's whole USDC delta (abi.md 6.2).
    Burn(CctpBurn),
    /// An Eco intent funded with the Runner's whole USDC delta, and the fee Eco keeps of
    /// what its route carries (abi.md 6.3).
    Eco { intent: EcoIntent, fee: U256 },
    /// A KyberSwap `swap` of the Runner's delta, its route opaque.
    Swap(KyberSwap),
}

/// Why a step is not one the canister reads.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum StepError {
    #[error("the step is none of the shapes the canister reads")]
    Unknown,
    #[error("the step does not decode as its shape")]
    Decode,
    #[error("the step's {0} is not exactly what its shape builds")]
    NotCanonical(&'static str),
    #[error(transparent)]
    Intent(#[from] NotAnIntent),
}

/// What a step is, read by parse, rebuild, compare: its calldata is decoded strictly into
/// its shape's parameters, rebuilt from them exactly as the quoter builds the shape (every
/// amount the Runner writes left at zero), and must be the same bytes; its writes must be
/// exactly the shape's own, at its offsets and with its `minus`. So a write can land only
/// on an amount word, and nothing the screen reads (a recipient, a creator, a target) can
/// be other at run time than it was when it was read. `usdc` and `messenger` are the
/// source chain's.
pub fn read_step(
    step: &Step,
    usdc: EvmAddress,
    messenger: EvmAddress,
) -> Result<KnownStep, StepError> {
    let same = |rebuilt: &[u8], what: &'static str| {
        if rebuilt == step.data.as_ref() {
            Ok(())
        } else {
            Err(StepError::NotCanonical(what))
        }
    };
    match selector_of(step) {
        Some(selectors::TRANSFER) if step.target == addr(usdc) => {
            let call = transferCall::abi_decode_with_config(&step.data, strict())
                .map_err(|_| StepError::Decode)?;
            same(&call.abi_encode(), "calldata")?;
            if step.token != Address::ZERO || !step.min.is_zero() || !step.writes.is_empty() {
                return Err(StepError::NotCanonical("token, min or writes"));
            }
            Ok(KnownStep::Transfer {
                to: evm(call.to),
                amount: call.amount,
            })
        }
        Some(selectors::DEPOSIT_FOR_BURN_WITH_HOOK) if step.target == addr(messenger) => {
            let call = depositForBurnWithHookCall::abi_decode_with_config(&step.data, strict())
                .map_err(|_| StepError::Decode)?;
            let burn = CctpBurn {
                destination_domain: call.destinationDomain,
                mint_recipient: call.mintRecipient.0,
                burn_token: evm(call.burnToken),
                destination_caller: call.destinationCaller.0,
                hook_data: call.hookData.to_vec(),
                max_fee: call.maxFee,
            };
            let rebuilt = depositForBurnWithHookCall {
                amount: U256::ZERO,
                ..call
            }
            .abi_encode();
            same(&rebuilt, "calldata")?;
            if step.token != addr(usdc) {
                return Err(StepError::NotCanonical("token"));
            }
            if step.writes
                != [Write {
                    at: offsets::CCTP_AMOUNT,
                    minus: U256::ZERO,
                }]
            {
                return Err(StepError::NotCanonical("writes"));
            }
            Ok(KnownStep::Burn(burn))
        }
        Some(PUBLISH_AND_FUND) => {
            let (intent, rebuilt, route_len) = EcoIntent::read(&step.data)?;
            same(&rebuilt, "calldata")?;
            if step.token != addr(usdc) {
                return Err(StepError::NotCanonical("token"));
            }
            let fee = step
                .writes
                .get(1)
                .map(|write| write.minus)
                .ok_or(StepError::NotCanonical("writes"))?;
            if step.writes != eco_writes(&intent.arrival, route_len, fee) {
                return Err(StepError::NotCanonical("writes"));
            }
            Ok(KnownStep::Eco { intent, fee })
        }
        Some(KYBER_SWAP) => {
            let call = swapCall::abi_decode_with_config(&step.data, strict())
                .map_err(|_| StepError::Decode)?;
            same(&call.abi_encode(), "calldata")?;
            if !step.writes.is_empty() {
                return Err(StepError::NotCanonical("writes"));
            }
            Ok(KnownStep::Swap(KyberSwap::of_call(call)))
        }
        _ => Err(StepError::Unknown),
    }
}

/// KyberSwap's `swap` selector.
pub const KYBER_SWAP: [u8; 4] = swapCall::SELECTOR;

/// KyberSwap's MetaAggregationRouterV2, at one address on every chain the quoter routes
/// on: the one swap target a gasless order may name.
pub const KYBER_ROUTER: EvmAddress = EvmAddress::new([
    0x61, 0x31, 0xb5, 0xfa, 0xe1, 0x9e, 0xa4, 0xf9, 0xd9, 0x64, 0xea, 0xc0, 0x40, 0x8e, 0x44, 0x08,
    0xb6, 0x63, 0x37, 0xb5,
]);

/// KyberSwap's executor, at one address on every chain the quoter routes on: the one
/// `callTarget` a swap may hand its input to. Should KyberSwap move it, gasless swaps are
/// refused until this follows.
pub const KYBER_EXECUTOR: EvmAddress = EvmAddress::new([
    0x8f, 0x10, 0xb4, 0x68, 0xb0, 0x6c, 0x6f, 0xd2, 0x14, 0xb6, 0x5f, 0x87, 0x77, 0x88, 0x27, 0xf7,
    0xd1, 0x13, 0xf9, 0x96,
]);

/// A KyberSwap `swap` an order makes, read back: who gets its input, its output and its
/// fees. The executor's route inside `targetData` is not read: which pools a swap may
/// cross is the quoter's rule (abi.md rule 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KyberSwap {
    pub src_token: EvmAddress,
    pub dst_token: EvmAddress,
    /// The executor the router calls with the route.
    pub call_target: EvmAddress,
    /// Who the router hands the input to.
    pub src_receivers: Vec<EvmAddress>,
    pub fee_receivers: Vec<EvmAddress>,
    /// Who the router pays the output to.
    pub dst_receiver: EvmAddress,
    /// Whether the swap carries a permit of its own.
    pub permit: bool,
}

/// A step to KyberSwap's `swap` this canister cannot read.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a KyberSwap swap step does not decode")]
pub struct NotASwap;

impl KyberSwap {
    /// The swap a step makes: `None` for a step that is no KyberSwap `swap`.
    pub fn of_step(step: &Step) -> Option<Result<Self, NotASwap>> {
        (selector_of(step) == Some(KYBER_SWAP)).then(|| {
            let call =
                swapCall::abi_decode_with_config(&step.data, strict()).map_err(|_| NotASwap)?;
            Ok(Self::of_call(call))
        })
    }

    fn of_call(call: swapCall) -> Self {
        let execution = call.execution;
        let desc = execution.desc;
        Self {
            src_token: evm(desc.srcToken),
            dst_token: evm(desc.dstToken),
            call_target: evm(execution.callTarget),
            src_receivers: desc.srcReceivers.into_iter().map(evm).collect(),
            fee_receivers: desc.feeReceivers.into_iter().map(evm).collect(),
            dst_receiver: evm(desc.dstReceiver),
            permit: !desc.permit.is_empty(),
        }
    }
}

/// The ICP principal a CkDeposit's envelope names: the ck helper's encoding, its length in
/// the first byte, its bytes after it, zeros after them. Nothing for a word that is not one.
pub fn ck_principal(word: [u8; 32]) -> Option<candid::Principal> {
    let len = usize::from(word[0]);
    if len == 0 || len > 29 || word[1 + len..].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(candid::Principal::from_slice(&word[1..1 + len]))
}

/// What an arrival does (`Action` in the contracts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// Minted to the recipient as it is: `receiveMessage`, no Swapic contract.
    Deliver,
    /// Swapped by the destination Runner's recipe into `tokenOut`.
    Swap,
    /// Deposited into the ck helper for an ICP principal.
    CkDeposit,
}

impl Action {
    pub fn code(self) -> u8 {
        match self {
            Self::Deliver => 0,
            Self::Swap => 1,
            Self::CkDeposit => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Deliver),
            1 => Some(Self::Swap),
            2 => Some(Self::CkDeposit),
            _ => None,
        }
    }
}

/// The sealed arrival instruction (`Envelope.sol`): `abi.encode` of 10 static words, 320
/// bytes, word i at byte 32 * i. In a CCTP v2 message it starts at byte 376.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// `Order.ref`.
    pub reference: [u8; 32],
    pub action: Action,
    /// The end of the canister-or-recipient window of a CCTP arrival, unix seconds.
    pub deadline: u64,
    /// The tokenOut receiver, and the refund receiver.
    pub recipient: EvmAddress,
    /// Swap only; `NATIVE` for ether.
    pub token_out: EvmAddress,
    /// Swap only, in tokenOut.
    pub min_out: U256,
    /// CkDeposit only.
    pub principal: [u8; 32],
    pub subaccount: [u8; 32],
    /// The gas `fill` must have left for the action.
    pub min_gas: u64,
}

/// Why bytes are not an envelope the contracts take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EnvelopeError {
    #[error("an envelope is {expected} bytes, not {0}", expected = Envelope::LENGTH)]
    Length(usize),
    #[error("envelope version {0}, not 3")]
    Version(U256),
    #[error("action {0} is not deliver, swap or ckDeposit")]
    Action(U256),
    #[error("envelope word {0} is out of its type's range")]
    Range(usize),
}

impl Envelope {
    pub const VERSION: u8 = 3;
    pub const LENGTH: usize = 320;
    /// Where the envelope starts in a CCTP v2 message: the body at 148, its hook data at
    /// 228 inside it.
    pub const CCTP_OFFSET: usize = 376;
    /// A Swapic CCTP message: the header, the burn body and the envelope.
    pub const CCTP_MESSAGE_LENGTH: usize = Self::CCTP_OFFSET + Self::LENGTH;

    /// The 320 bytes, word by word.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LENGTH);
        out.extend_from_slice(&word(U256::from(Self::VERSION)));
        out.extend_from_slice(&self.reference);
        out.extend_from_slice(&word(U256::from(self.action.code())));
        out.extend_from_slice(&word(U256::from(self.deadline)));
        out.extend_from_slice(&self.recipient.to_word());
        out.extend_from_slice(&self.token_out.to_word());
        out.extend_from_slice(&word(self.min_out));
        out.extend_from_slice(&self.principal);
        out.extend_from_slice(&self.subaccount);
        out.extend_from_slice(&word(U256::from(self.min_gas)));
        out
    }

    /// Exactly 320 bytes, version 3, every narrow word clean: what `Envelope.decode`
    /// accepts, and nothing else.
    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() != Self::LENGTH {
            return Err(EnvelopeError::Length(bytes.len()));
        }
        let raw = |i: usize| -> [u8; 32] {
            bytes[32 * i..32 * (i + 1)]
                .try_into()
                .expect("BUG: the length was checked")
        };
        let value = |i: usize| U256::from_be_bytes(raw(i));
        if value(0) != U256::from(Self::VERSION) {
            return Err(EnvelopeError::Version(value(0)));
        }
        let action = u8::try_from(value(2))
            .ok()
            .and_then(Action::from_code)
            .ok_or(EnvelopeError::Action(value(2)))?;
        let narrow = |i: usize| u64::try_from(value(i)).map_err(|_| EnvelopeError::Range(i));
        let address = |i: usize| {
            let w = raw(i);
            if w[..12].iter().any(|byte| *byte != 0) {
                return Err(EnvelopeError::Range(i));
            }
            Ok(EvmAddress::new(
                w[12..].try_into().expect("BUG: twenty bytes"),
            ))
        };
        Ok(Self {
            reference: raw(1),
            action,
            deadline: narrow(3)?,
            recipient: address(4)?,
            token_out: address(5)?,
            min_out: value(6),
            principal: raw(7),
            subaccount: raw(8),
            min_gas: narrow(9)?,
        })
    }

    /// The envelope a CCTP v2 message carries at its hook data, if it carries one of ours.
    pub fn of_cctp_message(message: &[u8]) -> Result<Self, EnvelopeError> {
        Self::decode(message.get(Self::CCTP_OFFSET..).unwrap_or_default())
    }
}

/// The ABI's word for an address.
pub fn addr(address: EvmAddress) -> Address {
    Address::from(*address.as_bytes())
}

/// This canister's address type, from the ABI's.
pub fn evm(address: Address) -> EvmAddress {
    EvmAddress::new(address.into_array())
}

/// The low 20 bytes of a bytes32, when its high 12 bytes are zero: an EVM address.
pub fn evm_of_word(word: [u8; 32]) -> Option<EvmAddress> {
    if word[..12].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(EvmAddress::new(
        word[12..].try_into().expect("BUG: twenty bytes"),
    ))
}

/// A bytes32 as the ABI holds it.
pub fn bytes32(word: [u8; 32]) -> FixedBytes<32> {
    FixedBytes(word)
}
