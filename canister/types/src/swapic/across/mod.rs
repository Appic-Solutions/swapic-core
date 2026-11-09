//! Across (BSC design 6.2.5 and 7.1 to 7.4): the identity proportional deposit step and its
//! arrival message, read the way every step is read: decoded strictly, rebuilt from the parsed
//! fields with every amount the Runner writes at zero, and compared byte for byte. The
//! canister's mirror of the quoter's builder (`libraries/onchain/src/across`), held to the bytes
//! a relayer filled in the live test of 2026-11-08.

#[cfg(test)]
mod tests;

use super::{addr, evm, fillCall, strict, Envelope, Recipe, Step, StepError, Write};
use crate::config::AcrossPins;
use crate::evm::EvmAddress;
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolValue};

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
