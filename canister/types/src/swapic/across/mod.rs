//! Across (BSC design 6.2.5 and 7.1 to 7.4): the identity proportional deposit step and its
//! arrival message, read the way every step is read: decoded strictly, rebuilt from the parsed
//! fields with every amount the Runner writes at zero, and compared byte for byte. The
//! canister's mirror of the quoter's builder (`libraries/onchain/src/across`), held to the bytes
//! a relayer filled in the live test of 2026-11-08.

#[cfg(test)]
mod tests;

use super::{addr, evm, fillCall, strict, Envelope, Recipe, Step, StepError, Write};
use crate::chain::ChainId;
use crate::config::AcrossPins;
use crate::evm::EvmAddress;
use crate::numeric::TokenAmount;
use alloy_primitives::{keccak256, Address, FixedBytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use minicbor::{Decode, Encode};

sol! {
    struct Fees { uint256 amount; address recipient; }

    struct BaseDepositData {
        address inputToken;
        bytes32 outputToken;
        uint256 outputAmount;
        address depositor;
        bytes32 recipient;
        uint256 destinationChainId;
        bytes32 exclusiveRelayer;
        uint32 quoteTimestamp;
        uint32 fillDeadline;
        uint32 exclusivityParameter;
        bytes message;
    }

    struct SwapAndDepositData {
        Fees submissionFees;
        BaseDepositData depositData;
        address swapToken;
        address exchange;
        uint8 transferType;
        uint256 swapTokenAmount;
        uint256 minExpectedInputTokenAmount;
        bytes routerCalldata;
        bool enableProportionalAdjustment;
        address spokePool;
        uint256 nonce;
    }

    /// SpokePoolPeriphery (selector 0x110560ad).
    function swapAndBridge(SwapAndDepositData swapAndDepositData) external payable;

    struct HandlerCall { address target; bytes callData; uint256 value; }
    struct Instructions { HandlerCall[] calls; address fallbackRecipient; }
    struct Replacement { address token; uint256 offset; }
    /// MulticallHandler: ORs the handler's balance of each token into the call data at its
    /// offset, then calls.
    function makeCallWithBalance(address target, bytes callData, uint256 value, Replacement[] replacement) external;

    function approve(address spender, uint256 amount) external returns (bool);
    function balanceOf(address owner) external view returns (uint256);
}

/// `swapAndBridge`'s selector: the step shape the canister reads as an Across deposit.
pub const SWAP_AND_BRIDGE: [u8; 4] = swapAndBridgeCall::SELECTOR;
/// Where the Runner writes the whole stable delta: `swapTokenAmount` (design 7.1).
pub const SWAP_TOKEN_AMOUNT_AT: u32 = 228;
/// Where `makeCallWithBalance` writes the handler's balance: the amount word of both `approve`
/// and `fill`.
pub const BALANCE_AT: u64 = 36;
/// The most `minGas` an arrival may name (D18): a larger one would make a plan unfillable.
pub const MAX_MIN_GAS: u64 = 2_000_000;

/// One Across deposit as an order signs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcrossDeposit {
    pub periphery: EvmAddress,
    pub spoke_pool: EvmAddress,
    /// The source chain's stable, swapped "into itself" (the identity) and deposited whole.
    pub input_token: EvmAddress,
    pub output_token: EvmAddress,
    /// F: what the deposit promises for an input of `min_expected`; the periphery scales it.
    pub output_amount: U256,
    /// M: the least the step deposits.
    pub min_expected: U256,
    pub depositor: EvmAddress,
    pub recipient: EvmAddress,
    pub destination_chain_id: u64,
    pub exclusive_relayer: EvmAddress,
    pub quote_timestamp: u32,
    pub fill_deadline: u32,
    /// Across's exclusivity: an offset in seconds, 0 with no relayer.
    pub exclusivity: u32,
    pub message: Vec<u8>,
}

impl AcrossDeposit {
    fn data(&self) -> SwapAndDepositData {
        SwapAndDepositData {
            submissionFees: Fees {
                amount: U256::ZERO,
                recipient: Address::ZERO,
            },
            depositData: BaseDepositData {
                inputToken: addr(self.input_token),
                outputToken: addr(self.output_token).into_word(),
                outputAmount: self.output_amount,
                depositor: addr(self.depositor),
                recipient: addr(self.recipient).into_word(),
                destinationChainId: U256::from(self.destination_chain_id),
                exclusiveRelayer: addr(self.exclusive_relayer).into_word(),
                quoteTimestamp: self.quote_timestamp,
                fillDeadline: self.fill_deadline,
                exclusivityParameter: self.exclusivity,
                message: self.message.clone().into(),
            },
            swapToken: addr(self.input_token),
            exchange: addr(self.input_token),
            transferType: 0,
            swapTokenAmount: U256::ZERO,
            minExpectedInputTokenAmount: self.min_expected,
            routerCalldata: balanceOfCall {
                owner: addr(self.periphery),
            }
            .abi_encode()
            .into(),
            enableProportionalAdjustment: true,
            spokePool: addr(self.spoke_pool),
            nonce: U256::ZERO,
        }
    }

    /// `swapAndBridge`'s calldata, `swapTokenAmount` zero for the Runner to write.
    pub fn calldata(&self) -> Vec<u8> {
        swapAndBridgeCall {
            swapAndDepositData: self.data(),
        }
        .abi_encode()
    }

    /// The step that makes this deposit (design 7.1).
    pub fn step(&self) -> Step {
        Step {
            target: addr(self.periphery),
            data: self.calldata().into(),
            token: addr(self.input_token),
            min: self.min_expected,
            writes: vec![Write {
                at: SWAP_TOKEN_AMOUNT_AT,
                minus: U256::ZERO,
            }],
        }
    }
}

/// An address word: its top 12 bytes zero.
fn word_address(word: B256) -> Result<EvmAddress, StepError> {
    if word[..12].iter().any(|byte| *byte != 0) {
        return Err(StepError::NotCanonical("address word"));
    }
    Ok(evm(Address::from_word(word)))
}

/// A step read as the Across deposit it makes, at the source chain's `pins` (rules AX1 to AX4):
/// its target the periphery, its data `swapAndBridge` rebuilt byte for byte from its fields with
/// `swapTokenAmount` zero (so the submission fees, the transfer type, the no-op router call,
/// the proportional flag and the nonce are the identity deposit's), its token, swap token,
/// exchange and input the one stable, its least M and the periphery's the same and above zero,
/// its one write at 228, and its SpokePool the pinned one.
pub fn read_across_step(step: &Step, pins: &AcrossPins) -> Result<AcrossDeposit, StepError> {
    if step.target != addr(pins.periphery) {
        return Err(StepError::NotCanonical("target"));
    }
    if step.data.get(..4) != Some(SWAP_AND_BRIDGE.as_slice()) {
        return Err(StepError::Unknown);
    }
    let call = swapAndBridgeCall::abi_decode_with_config(&step.data, strict())
        .map_err(|_| StepError::Decode)?;
    let d = call.swapAndDepositData;
    let deposit = AcrossDeposit {
        periphery: pins.periphery,
        spoke_pool: evm(d.spokePool),
        input_token: evm(d.depositData.inputToken),
        output_token: word_address(d.depositData.outputToken)?,
        output_amount: d.depositData.outputAmount,
        min_expected: d.minExpectedInputTokenAmount,
        depositor: evm(d.depositData.depositor),
        recipient: word_address(d.depositData.recipient)?,
        destination_chain_id: d
            .depositData
            .destinationChainId
            .try_into()
            .map_err(|_| StepError::NotCanonical("destinationChainId"))?,
        exclusive_relayer: word_address(d.depositData.exclusiveRelayer)?,
        quote_timestamp: d.depositData.quoteTimestamp,
        fill_deadline: d.depositData.fillDeadline,
        exclusivity: d.depositData.exclusivityParameter,
        message: d.depositData.message.to_vec(),
    };
    if deposit.calldata() != step.data.as_ref() {
        return Err(StepError::NotCanonical("calldata"));
    }
    if step.token != d.swapToken
        || d.swapToken != d.exchange
        || d.exchange != d.depositData.inputToken
    {
        return Err(StepError::NotCanonical("token"));
    }
    if step.min != deposit.min_expected || deposit.min_expected.is_zero() {
        return Err(StepError::NotCanonical("min"));
    }
    if step.writes
        != [Write {
            at: SWAP_TOKEN_AMOUNT_AT,
            minus: U256::ZERO,
        }]
    {
        return Err(StepError::NotCanonical("writes"));
    }
    if deposit.spoke_pool != pins.spoke_pool {
        return Err(StepError::NotCanonical("spokePool"));
    }
    Ok(deposit)
}

/// The arrival message (design 7.3): the handler approves `runner` its whole balance of
/// `stable`, then calls `Runner.fill` with it; `fallbackRecipient` zero, so a failure the Runner
/// does not answer itself reverts the fill (attack H2).
pub fn arrival_message(
    handler: EvmAddress,
    stable: EvmAddress,
    runner: EvmAddress,
    envelope: Vec<u8>,
    recipe: Recipe,
) -> Vec<u8> {
    let replace = vec![Replacement {
        token: addr(stable),
        offset: U256::from(BALANCE_AT),
    }];
    let approve = approveCall {
        spender: addr(runner),
        amount: U256::ZERO,
    }
    .abi_encode();
    let fill = fillCall {
        token: addr(stable),
        amount: U256::ZERO,
        envelope: envelope.into(),
        recipe,
    }
    .abi_encode();
    let call = |target: EvmAddress, data: Vec<u8>| HandlerCall {
        target: addr(handler),
        callData: makeCallWithBalanceCall {
            target: addr(target),
            callData: data.into(),
            value: U256::ZERO,
            replacement: replace.clone(),
        }
        .abi_encode()
        .into(),
        value: U256::ZERO,
    };
    Instructions {
        calls: vec![call(stable, approve), call(runner, fill)],
        fallbackRecipient: Address::ZERO,
    }
    .abi_encode()
}

/// The arrival message read back (rule AM1): two `makeCallWithBalance` calls on `handler`, the
/// second a `Runner.fill` of `stable`, rebuilt byte for byte from the envelope and the recipe it
/// carries; the envelope one the contracts decode. Returns them.
pub fn read_arrival(
    message: &[u8],
    handler: EvmAddress,
    stable: EvmAddress,
    runner: EvmAddress,
) -> Result<(Envelope, Recipe), StepError> {
    let bad = |_| StepError::NotCanonical("message");
    let instructions = Instructions::abi_decode_with_config(message, strict()).map_err(bad)?;
    let [_, second] = instructions.calls.as_slice() else {
        return Err(StepError::NotCanonical("message"));
    };
    let outer =
        makeCallWithBalanceCall::abi_decode_with_config(&second.callData, strict()).map_err(bad)?;
    let fill = fillCall::abi_decode_with_config(&outer.callData, strict()).map_err(bad)?;
    let rebuilt = arrival_message(
        handler,
        stable,
        runner,
        fill.envelope.to_vec(),
        fill.recipe.clone(),
    );
    if rebuilt != message {
        return Err(StepError::NotCanonical("message"));
    }
    let envelope =
        Envelope::decode(&fill.envelope).map_err(|_| StepError::NotCanonical("envelope"))?;
    Ok((envelope, fill.recipe))
}

/// The integrator suffix (design 7.6, D13): `0x1dc0de` and the 2-byte id.
pub fn integrator_suffix(id: u16) -> [u8; 5] {
    let [hi, lo] = id.to_be_bytes();
    [0x1d, 0xc0, 0xde, hi, lo]
}

impl AcrossDeposit {
    /// This deposit fitted to the payout's exact `amount` (BSC design 7.5): its least M and
    /// the Runner's step minimum become `amount`, its output F the same ratio of it,
    /// `floor(F * amount / M)`, as the periphery itself scales an input above M. Every other
    /// byte stays. None when the product leaves 256 bits.
    pub fn fitted(&self, amount: U256) -> Option<AcrossDeposit> {
        let output_amount = self
            .output_amount
            .checked_mul(amount)?
            .checked_div(self.min_expected)?;
        Some(AcrossDeposit {
            output_amount,
            min_expected: amount,
            ..self.clone()
        })
    }
}

/// An Across step fitted to a payout's exact `amount` (BSC design 7.5): decoded strictly as
/// the identity deposit to its own target and SpokePool, its least M and output F rewritten
/// ([`AcrossDeposit::fitted`]), every other byte as it was. The order's own checks then read
/// it again against the pins.
pub fn fit_step(step: &Step, amount: U256) -> Result<Step, StepError> {
    let call = swapAndBridgeCall::abi_decode_with_config(&step.data, strict())
        .map_err(|_| StepError::Decode)?;
    let pins = AcrossPins {
        periphery: evm(step.target),
        swap_proxy: EvmAddress::ZERO,
        spoke_pool: evm(call.swapAndDepositData.spokePool),
        multicall_handler: EvmAddress::ZERO,
    };
    let deposit = read_across_step(step, &pins)?;
    let fitted = deposit
        .fitted(amount)
        .ok_or(StepError::NotCanonical("outputAmount"))?;
    Ok(fitted.step())
}

/// Where the one Across step of an order stands, by its selector; None for none, or for two.
pub fn across_step_at(steps: &[Step]) -> Option<usize> {
    let mut at = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.data.get(..4) == Some(SWAP_AND_BRIDGE.as_slice()))
        .map(|(index, _)| index);
    let first = at.next()?;
    at.next().is_none().then_some(first)
}

sol! {
    /// The source SpokePool's record of a deposit.
    event FundsDeposited(
        bytes32 inputToken,
        bytes32 outputToken,
        uint256 inputAmount,
        uint256 outputAmount,
        uint256 indexed destinationChainId,
        uint256 indexed depositId,
        uint32 quoteTimestamp,
        uint32 fillDeadline,
        uint32 exclusivityDeadline,
        bytes32 indexed depositor,
        bytes32 recipient,
        bytes32 exclusiveRelayer,
        bytes message
    );

    struct V3RelayExecutionEventInfo {
        bytes32 updatedRecipient;
        bytes32 updatedMessageHash;
        uint256 updatedOutputAmount;
        uint8 fillType;
    }

    /// The destination SpokePool's record of a fill.
    event FilledRelay(
        bytes32 inputToken,
        bytes32 outputToken,
        uint256 inputAmount,
        uint256 outputAmount,
        uint256 repaymentChainId,
        uint256 indexed originChainId,
        uint256 indexed depositId,
        uint32 fillDeadline,
        uint32 exclusivityDeadline,
        bytes32 exclusiveRelayer,
        bytes32 indexed relayer,
        bytes32 depositor,
        bytes32 recipient,
        bytes32 messageHash,
        V3RelayExecutionEventInfo relayExecutionInfo
    );

    /// The periphery's record of the swap before a deposit: what it pulled and what it put in.
    event SwapBeforeBridge(
        address exchange,
        bytes exchangeCalldata,
        address indexed swapToken,
        address indexed acrossInputToken,
        uint256 swapTokenAmount,
        uint256 acrossInputAmount,
        bytes32 indexed acrossOutputToken,
        uint256 acrossOutputAmount
    );

    /// A relayer refund leaf paid out: an expired deposit's refund to its depositor among them.
    event ExecutedRelayerRefundRoot(
        uint256 amountToReturn,
        uint256 indexed chainId,
        uint256[] refundAmounts,
        uint32 indexed rootBundleId,
        uint32 indexed leafId,
        address l2TokenAddress,
        address[] refundAddresses,
        bool deferredRefunds,
        address caller
    );

    /// A deferred refund claimed: paid to `refundAddress` by whoever called.
    event ClaimedRelayerRefund(
        bytes32 indexed l2TokenAddress,
        bytes32 indexed refundAddress,
        uint256 amount,
        address indexed caller
    );

    struct V3RelayData {
        bytes32 depositor;
        bytes32 recipient;
        bytes32 exclusiveRelayer;
        bytes32 inputToken;
        bytes32 outputToken;
        uint256 inputAmount;
        uint256 outputAmount;
        uint256 originChainId;
        uint256 depositId;
        uint32 fillDeadline;
        uint32 exclusivityDeadline;
        bytes message;
    }

    function getV3RelayHash(V3RelayData relayData) external view returns (bytes32);
    function fillStatuses(bytes32 relayHash) external view returns (uint256);
    function getRelayerRefund(address l2TokenAddress, address refundAddress) external view returns (uint256);
    function claimRelayerRefund(bytes32 l2TokenAddress, bytes32 refundAddress) external;
}

/// The events of Across's contracts the canister reads, by their first topic.
pub mod topics {
    use super::*;
    use alloy_sol_types::SolEvent;

    pub fn funds_deposited() -> [u8; 32] {
        FundsDeposited::SIGNATURE_HASH.0
    }

    pub fn filled_relay() -> [u8; 32] {
        FilledRelay::SIGNATURE_HASH.0
    }

    pub fn swap_before_bridge() -> [u8; 32] {
        SwapBeforeBridge::SIGNATURE_HASH.0
    }

    pub fn executed_relayer_refund_root() -> [u8; 32] {
        ExecutedRelayerRefundRoot::SIGNATURE_HASH.0
    }

    pub fn claimed_relayer_refund() -> [u8; 32] {
        ClaimedRelayerRefund::SIGNATURE_HASH.0
    }
}

/// A deposit's relay as the SpokePools name it (`V3RelayData`, V3SpokePoolInterface.sol
/// 38-66): every field the source's `FundsDeposited` logs, the deposit id and origin chain
/// with them. Addresses are words, as the SpokePools hold them. What a fill on the
/// destination is looked up by.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct RelayData {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub depositor: [u8; 32],
    #[cbor(n(1), with = "minicbor::bytes")]
    pub recipient: [u8; 32],
    #[cbor(n(2), with = "minicbor::bytes")]
    pub exclusive_relayer: [u8; 32],
    #[cbor(n(3), with = "minicbor::bytes")]
    pub input_token: [u8; 32],
    #[cbor(n(4), with = "minicbor::bytes")]
    pub output_token: [u8; 32],
    #[n(5)]
    pub input_amount: TokenAmount,
    #[n(6)]
    pub output_amount: TokenAmount,
    #[n(7)]
    pub origin_chain_id: ChainId,
    /// The source SpokePool's id of the deposit, a 256-bit number.
    #[n(8)]
    pub deposit_id: TokenAmount,
    #[n(9)]
    pub fill_deadline: u32,
    /// The time the exclusive relayer's turn ends (the log's, an instant, not the offset).
    #[n(10)]
    pub exclusivity_deadline: u32,
    #[cbor(n(11), with = "minicbor::bytes")]
    pub message: Vec<u8>,
}

/// A message as a fill names it: zero for none, its keccak otherwise (the SpokePool's own
/// rule).
pub fn message_hash(message: &[u8]) -> [u8; 32] {
    match message.is_empty() {
        true => [0; 32],
        false => keccak256(message).0,
    }
}

/// The fields a deposit and its fill both name: the relay, its message by hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayKey {
    pub depositor: [u8; 32],
    pub recipient: [u8; 32],
    pub exclusive_relayer: [u8; 32],
    pub input_token: [u8; 32],
    pub output_token: [u8; 32],
    pub input_amount: U256,
    pub output_amount: U256,
    pub origin_chain_id: u64,
    pub deposit_id: U256,
    pub fill_deadline: u32,
    pub exclusivity_deadline: u32,
    pub message_hash: [u8; 32],
}

impl RelayData {
    fn sol(&self) -> V3RelayData {
        V3RelayData {
            depositor: self.depositor.into(),
            recipient: self.recipient.into(),
            exclusiveRelayer: self.exclusive_relayer.into(),
            inputToken: self.input_token.into(),
            outputToken: self.output_token.into(),
            inputAmount: self.input_amount.to_u256(),
            outputAmount: self.output_amount.to_u256(),
            originChainId: U256::from(self.origin_chain_id.get()),
            depositId: self.deposit_id.to_u256(),
            fillDeadline: self.fill_deadline,
            exclusivityDeadline: self.exclusivity_deadline,
            message: self.message.clone().into(),
        }
    }

    /// The hash the destination SpokePool keys this relay's fill by:
    /// `keccak256(abi.encode(relayData, destinationChainId))`, as its `getV3RelayHash` answers
    /// on `destination` (held to T0's answer on Arbitrum).
    pub fn hash(&self, destination: ChainId) -> [u8; 32] {
        keccak256((self.sol(), U256::from(destination.get())).abi_encode_params()).0
    }

    /// `getV3RelayHash(relayData)`'s calldata.
    pub fn hash_calldata(&self) -> Vec<u8> {
        getV3RelayHashCall {
            relayData: self.sol(),
        }
        .abi_encode()
    }

    /// The fields its fill repeats.
    pub fn key(&self) -> RelayKey {
        RelayKey {
            depositor: self.depositor,
            recipient: self.recipient,
            exclusive_relayer: self.exclusive_relayer,
            input_token: self.input_token,
            output_token: self.output_token,
            input_amount: self.input_amount.to_u256(),
            output_amount: self.output_amount.to_u256(),
            origin_chain_id: self.origin_chain_id.get(),
            deposit_id: self.deposit_id.to_u256(),
            fill_deadline: self.fill_deadline,
            exclusivity_deadline: self.exclusivity_deadline,
            message_hash: message_hash(&self.message),
        }
    }
}

/// `fillStatuses(relayHash)`'s calldata.
pub fn fill_statuses_calldata(relay_hash: [u8; 32]) -> Vec<u8> {
    fillStatusesCall {
        relayHash: relay_hash.into(),
    }
    .abi_encode()
}

/// `getRelayerRefund(token, owner)`'s calldata: what the SpokePool holds back for `owner`.
pub fn get_relayer_refund_calldata(token: EvmAddress, owner: EvmAddress) -> Vec<u8> {
    getRelayerRefundCall {
        l2TokenAddress: addr(token),
        refundAddress: addr(owner),
    }
    .abi_encode()
}

/// `claimRelayerRefund(bytes32(token), bytes32(owner))`'s calldata: pays `owner` what the
/// SpokePool holds back for it.
pub fn claim_relayer_refund_calldata(token: EvmAddress, owner: EvmAddress) -> Vec<u8> {
    claimRelayerRefundCall {
        l2TokenAddress: addr(token).into_word(),
        refundAddress: addr(owner).into_word(),
    }
    .abi_encode()
}

/// What `fillStatuses` answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillStatus {
    Unfilled,
    RequestedSlowFill,
    Filled,
}

impl FillStatus {
    /// The status a word answers; None for anything else.
    pub fn of(word: [u8; 32]) -> Option<Self> {
        if word[..31] != [0; 31] {
            return None;
        }
        match word[31] {
            0 => Some(FillStatus::Unfilled),
            1 => Some(FillStatus::RequestedSlowFill),
            2 => Some(FillStatus::Filled),
            _ => None,
        }
    }
}

fn decoded<E: alloy_sol_types::SolEvent>(topics: &[[u8; 32]], data: &[u8]) -> Option<E> {
    E::decode_raw_log_with_config(topics.iter().map(|topic| FixedBytes(*topic)), data, strict())
        .ok()
}

/// One `FundsDeposited` read: the relay it opens (its origin `origin`, the chain the log is
/// on) and the chain it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deposited {
    pub relay: RelayData,
    pub destination: u64,
    pub quote_timestamp: u32,
}

/// A `FundsDeposited` log on `origin` read strictly; None for anything else.
pub fn read_funds_deposited(
    topics: &[[u8; 32]],
    data: &[u8],
    origin: ChainId,
) -> Option<Deposited> {
    let event: FundsDeposited = decoded(topics, data)?;
    Some(Deposited {
        relay: RelayData {
            depositor: event.depositor.0,
            recipient: event.recipient.0,
            exclusive_relayer: event.exclusiveRelayer.0,
            input_token: event.inputToken.0,
            output_token: event.outputToken.0,
            input_amount: TokenAmount::from_u256(event.inputAmount),
            output_amount: TokenAmount::from_u256(event.outputAmount),
            origin_chain_id: origin,
            deposit_id: TokenAmount::from_u256(event.depositId),
            fill_deadline: event.fillDeadline,
            exclusivity_deadline: event.exclusivityDeadline,
            message: event.message.to_vec(),
        },
        destination: event.destinationChainId.try_into().ok()?,
        quote_timestamp: event.quoteTimestamp,
    })
}

/// How a relay was filled (`FillType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillType {
    FastFill,
    ReplacedSlowFill,
    SlowFill,
}

/// One `FilledRelay` read: the relay it fills, who filled it and what it paid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filled {
    pub key: RelayKey,
    /// The relayer, a word: a fast fill's tokens come from it.
    pub relayer: [u8; 32],
    pub updated_recipient: [u8; 32],
    pub updated_message_hash: [u8; 32],
    pub updated_output_amount: U256,
    pub fill_type: FillType,
}

/// A `FilledRelay` log read strictly; None for anything else.
pub fn read_filled_relay(topics: &[[u8; 32]], data: &[u8]) -> Option<Filled> {
    let event: FilledRelay = decoded(topics, data)?;
    let info = event.relayExecutionInfo;
    let fill_type = match info.fillType {
        0 => FillType::FastFill,
        1 => FillType::ReplacedSlowFill,
        2 => FillType::SlowFill,
        _ => return None,
    };
    Some(Filled {
        key: RelayKey {
            depositor: event.depositor.0,
            recipient: event.recipient.0,
            exclusive_relayer: event.exclusiveRelayer.0,
            input_token: event.inputToken.0,
            output_token: event.outputToken.0,
            input_amount: event.inputAmount,
            output_amount: event.outputAmount,
            origin_chain_id: event.originChainId.try_into().ok()?,
            deposit_id: event.depositId,
            fill_deadline: event.fillDeadline,
            exclusivity_deadline: event.exclusivityDeadline,
            message_hash: event.messageHash.0,
        },
        relayer: event.relayer.0,
        updated_recipient: info.updatedRecipient.0,
        updated_message_hash: info.updatedMessageHash.0,
        updated_output_amount: info.updatedOutputAmount,
        fill_type,
    })
}

/// One `SwapBeforeBridge` read: what the periphery pulled of `swap_token` and what it
/// deposited of `input_token`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swapped {
    pub swap_token: EvmAddress,
    pub input_token: EvmAddress,
    pub swap_token_amount: U256,
    pub input_amount: U256,
}

/// A `SwapBeforeBridge` log read strictly; None for anything else.
pub fn read_swap_before_bridge(topics: &[[u8; 32]], data: &[u8]) -> Option<Swapped> {
    let event: SwapBeforeBridge = decoded(topics, data)?;
    Some(Swapped {
        swap_token: evm(event.swapToken),
        input_token: evm(event.acrossInputToken),
        swap_token_amount: event.swapTokenAmount,
        input_amount: event.acrossInputAmount,
    })
}

/// One refund leaf paid: its chain, its token, and each refund address with its amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefundLeaf {
    pub chain_id: u64,
    pub token: EvmAddress,
    pub refunds: Vec<(EvmAddress, TokenAmount)>,
    /// Paid later by a claim, not in this transaction.
    pub deferred: bool,
}

/// An `ExecutedRelayerRefundRoot` log read strictly; None for anything else, or for a leaf
/// whose two lists differ in length.
pub fn read_refund_root(topics: &[[u8; 32]], data: &[u8]) -> Option<RefundLeaf> {
    let event: ExecutedRelayerRefundRoot = decoded(topics, data)?;
    if event.refundAmounts.len() != event.refundAddresses.len() {
        return None;
    }
    Some(RefundLeaf {
        chain_id: event.chainId.try_into().ok()?,
        token: evm(event.l2TokenAddress),
        refunds: event
            .refundAddresses
            .iter()
            .zip(&event.refundAmounts)
            .map(|(to, amount)| (evm(*to), TokenAmount::from_u256(*amount)))
            .collect(),
        deferred: event.deferredRefunds,
    })
}

/// One deferred refund claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub token: [u8; 32],
    pub refund_address: [u8; 32],
    pub amount: TokenAmount,
    pub caller: EvmAddress,
}

/// A `ClaimedRelayerRefund` log read strictly; None for anything else.
pub fn read_claimed_refund(topics: &[[u8; 32]], data: &[u8]) -> Option<Claimed> {
    let event: ClaimedRelayerRefund = decoded(topics, data)?;
    Some(Claimed {
        token: event.l2TokenAddress.0,
        refund_address: event.refundAddress.0,
        amount: TokenAmount::from_u256(event.amount),
        caller: evm(event.caller),
    })
}
