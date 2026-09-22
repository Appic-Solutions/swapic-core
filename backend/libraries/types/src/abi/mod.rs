//! The calldata of every call this canister sends: the vault's four doors, CCTP v2's two
//! and Eco's two. One builder per call, taking the domain's own types, so no caller
//! assembles a selector or a word by hand, and one decoder per call that this canister
//! broadcasts, so a test can read the bytes it sent back into the call they make.
//!
//! The signatures are declared once, in Solidity, and `alloy-sol-types` derives both the
//! selector and the encoding from them. Each unit test pins the result against
//! `cast calldata`, with the command that produced it above the fixture.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::hash::QuoteHash;
use crate::numeric::{Permit2Nonce, TokenAmount, UnixSeconds, Wei};
use alloy_primitives::{keccak256, Address, FixedBytes, I256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};

sol! {
    /// One router call inside a vault execution.
    struct Call {
        address target;
        uint256 value;
        bytes data;
        address approveToken;
        uint256 approveAmount;
    }

    /// The minimum balance change a vault execution must leave behind.
    struct Delta {
        address token;
        int256 minChange;
    }

    /// What every vault entry door logs: the quote the funds are for, the token, the
    /// payer, and the amount the vault measured as received.
    event Deposited(bytes32 indexed quoteHash, address indexed token, address indexed from, uint256 amount);

    /// What CCTP v2's token messenger logs when a message is received: the recipient, the
    /// amount minted to it (the burn less the fee), the token, and the fee Circle kept.
    event MintAndWithdraw(address indexed mintRecipient, uint256 amount, address indexed mintToken, uint256 feeCollected);

    function execute(bytes32 swapRef, Call[] calls, Delta[] deltas);
    function payout(bytes32 swapRef, address token, address to, uint256 amount);
    function refund(bytes32 ref, address token, address to, uint256 amount);
    function pullWithPermit(
        bytes32 quoteHash,
        address token,
        address owner,
        uint256 amount,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    );
    /// What a Permit2 signature permits: one token, up to one amount.
    struct TokenPermissions {
        address token;
        uint256 amount;
    }
    /// The Permit2 permit the vault hands to Permit2, exactly as the user signed it. The
    /// spender is not in it: Permit2 takes the caller, which is the vault.
    struct PermitTransferFrom {
        TokenPermissions permitted;
        uint256 nonce;
        uint256 deadline;
    }
    function pullWithPermit2(
        bytes32 quoteHash,
        address owner,
        PermitTransferFrom permit,
        bytes signature
    );
    function depositForBurn(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold
    );
    function receiveMessage(bytes message, bytes attestation);
}

/// Eco's Portal, in its own block: its `refund` shares a name with the vault's and the two
/// are different calls.
mod portal {
    use alloy_sol_types::sol;

    sol! {
        /// What Eco pays whoever fills an intent: the tokens the source vault locks in the
        /// Portal until `deadline`, refundable to `creator` after it.
        struct TokenAmountSol {
            address token;
            uint256 amount;
        }

        struct Reward {
            uint64 deadline;
            address creator;
            address prover;
            uint256 nativeAmount;
            TokenAmountSol[] tokens;
        }

        function publishAndFund(uint64 destination, bytes route, Reward reward, bool allowPartial);
        function refund(uint64 destination, bytes32 routeHash, Reward reward);
    }
}

use portal::{publishAndFundCall, refundCall as ecoRefundCall, Reward, TokenAmountSol};

/// One router call the vault runs on this canister's behalf: the vault approves
/// `approve_amount` of `approve_token` to `target`, calls it, and zeroes the approval
/// again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultCall {
    pub target: EvmAddress,
    pub value: Wei,
    pub data: Vec<u8>,
    pub approve_token: EvmAddress,
    pub approve_amount: TokenAmount,
}

/// The least a token's balance may change over an execution. Signed, because a leg that
/// spends a token declares a floor below zero; held as an `i128`, which covers every
/// amount this canister ever asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultDelta {
    pub token: EvmAddress,
    pub min_change: i128,
}

/// A Permit2 `PermitTransferFrom` as the user signed it: one token, one amount, the nonce
/// Permit2 spends and the deadline it holds the signature to. The witness the signature
/// also commits to is the quote hash, which the vault computes from the hash it is called
/// with, and the spender is the vault, which Permit2 takes from its caller: neither is in
/// this struct, because neither is in the calldata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permit2Permit {
    pub token: EvmAddress,
    pub amount: TokenAmount,
    pub nonce: Permit2Nonce,
    pub deadline: UnixSeconds,
}

/// An EIP-2612 permit signature, as the token contract takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permit {
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

/// A CCTP v2 burn. `mint_recipient` and `destination_caller` are 32-byte words because
/// CCTP addresses are not all 20 bytes; [`EvmAddress::to_word`] makes one from an EVM
/// address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Burn {
    pub amount: TokenAmount,
    pub destination_domain: u32,
    pub mint_recipient: [u8; 32],
    pub burn_token: EvmAddress,
    /// The zero word lets anyone deliver the mint.
    pub destination_caller: [u8; 32],
    /// The most of `amount` the fast path may take as its fee.
    pub max_fee: TokenAmount,
    /// 1000 is CCTP v2's fast threshold, 2000 its standard one.
    pub min_finality_threshold: u32,
}

/// What Eco's Portal locks for an intent and pays to its filler: the reward, with the
/// vault as its creator so a refund after `deadline` lands back in the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcoReward {
    pub deadline: UnixSeconds,
    pub creator: EvmAddress,
    pub prover: EvmAddress,
    pub native_amount: Wei,
    pub tokens: Vec<(EvmAddress, TokenAmount)>,
}

fn address(address: EvmAddress) -> Address {
    Address::from(*address.as_bytes())
}

fn evm_address(address: Address) -> EvmAddress {
    EvmAddress::new(address.0 .0)
}

fn amount<Unit>(amount: crate::checked_amount::CheckedAmountOf<Unit>) -> U256 {
    U256::from_be_bytes(amount.to_be_bytes())
}

fn checked<Unit>(value: U256) -> crate::checked_amount::CheckedAmountOf<Unit> {
    crate::checked_amount::CheckedAmountOf::from_be_bytes(value.to_be_bytes())
}

fn word(bytes: [u8; 32]) -> FixedBytes<32> {
    FixedBytes(bytes)
}

fn reward(reward: &EcoReward) -> Reward {
    Reward {
        deadline: reward.deadline.get(),
        creator: address(reward.creator),
        prover: address(reward.prover),
        nativeAmount: amount(reward.native_amount),
        tokens: reward
            .tokens
            .iter()
            .map(|(token, value)| TokenAmountSol {
                token: address(*token),
                amount: amount(*value),
            })
            .collect(),
    }
}

fn eco_reward(reward: Reward) -> EcoReward {
    EcoReward {
        deadline: UnixSeconds::new(reward.deadline),
        creator: evm_address(reward.creator),
        prover: evm_address(reward.prover),
        native_amount: checked(reward.nativeAmount),
        tokens: reward
            .tokens
            .into_iter()
            .map(|token| (evm_address(token.token), checked(token.amount)))
            .collect(),
    }
}

/// The topic a `Deposited` log carries first: the keccak of
/// `Deposited(bytes32,address,address,uint256)`, which is what the deposit read filters
/// the vault's logs by.
pub fn deposited_topic() -> [u8; 32] {
    Deposited::SIGNATURE_HASH.0
}

/// The topic a `MintAndWithdraw` log carries first: the keccak of
/// `MintAndWithdraw(address,uint256,address,uint256)`, which is what the mint read finds
/// in a mint's receipt to learn what the destination vault was delivered.
pub fn mint_and_withdraw_topic() -> [u8; 32] {
    MintAndWithdraw::SIGNATURE_HASH.0
}

/// `execute(bytes32,(address,uint256,bytes,address,uint256)[],(address,int256)[])`.
pub fn vault_execute(swap_ref: QuoteHash, calls: &[VaultCall], deltas: &[VaultDelta]) -> Vec<u8> {
    executeCall {
        swapRef: word(swap_ref.into_bytes()),
        calls: calls
            .iter()
            .map(|call| Call {
                target: address(call.target),
                value: amount(call.value),
                data: call.data.clone().into(),
                approveToken: address(call.approve_token),
                approveAmount: amount(call.approve_amount),
            })
            .collect(),
        deltas: deltas
            .iter()
            .map(|delta| Delta {
                token: address(delta.token),
                minChange: I256::try_from(delta.min_change)
                    .expect("BUG: every i128 is inside an int256"),
            })
            .collect(),
    }
    .abi_encode()
}

/// `payout(bytes32,address,address,uint256)`.
pub fn vault_payout(
    swap_ref: QuoteHash,
    token: EvmAddress,
    to: EvmAddress,
    value: TokenAmount,
) -> Vec<u8> {
    payoutCall {
        swapRef: word(swap_ref.into_bytes()),
        token: address(token),
        to: address(to),
        amount: amount(value),
    }
    .abi_encode()
}

/// `refund(bytes32,address,address,uint256)`.
pub fn vault_refund(
    swap_ref: QuoteHash,
    token: EvmAddress,
    to: EvmAddress,
    value: TokenAmount,
) -> Vec<u8> {
    refundCall {
        r#ref: word(swap_ref.into_bytes()),
        token: address(token),
        to: address(to),
        amount: amount(value),
    }
    .abi_encode()
}

/// `pullWithPermit(bytes32,address,address,uint256,uint256,uint8,bytes32,bytes32)`.
///
/// NOT A DOOR THIS CANISTER USES. The vault's 2612 path wraps the permit in `try/catch`
/// and transfers on whatever allowance the owner already gave the vault, so the signature
/// decides nothing, and a 2612 permit names `(owner, spender, value, deadline)` and never
/// a quote, so one signed for a quote can be spent under another. The canister pulls
/// through [`vault_pull_with_permit2`] only. The encoder stays because the test below pins
/// the vault's ABI, and for a caller outside this canister.
pub fn vault_pull_with_permit(
    quote_hash: QuoteHash,
    token: EvmAddress,
    owner: EvmAddress,
    value: TokenAmount,
    deadline: UnixSeconds,
    permit: &Permit,
) -> Vec<u8> {
    pullWithPermitCall {
        quoteHash: word(quote_hash.into_bytes()),
        token: address(token),
        owner: address(owner),
        amount: amount(value),
        deadline: U256::from(deadline.get()),
        v: permit.v,
        r: word(permit.r),
        s: word(permit.s),
    }
    .abi_encode()
}

/// `pullWithPermit2(bytes32,address,((address,uint256),uint256,uint256),bytes)`: the pull
/// this canister sends. The vault passes `quote_hash` to Permit2 as the witness the user's
/// signature commits to, and Permit2 verifies the signature itself, with no `try/catch`,
/// so a permit signed for one quote moves nothing under another.
pub fn vault_pull_with_permit2(
    quote_hash: QuoteHash,
    owner: EvmAddress,
    permit: &Permit2Permit,
    signature: &[u8],
) -> Vec<u8> {
    pullWithPermit2Call {
        quoteHash: word(quote_hash.into_bytes()),
        owner: address(owner),
        permit: PermitTransferFrom {
            permitted: TokenPermissions {
                token: address(permit.token),
                amount: amount(permit.amount),
            },
            nonce: amount(permit.nonce),
            deadline: U256::from(permit.deadline.get()),
        },
        signature: signature.to_vec().into(),
    }
    .abi_encode()
}

/// `depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32)`.
pub fn cctp_deposit_for_burn(burn: &Burn) -> Vec<u8> {
    depositForBurnCall {
        amount: amount(burn.amount),
        destinationDomain: burn.destination_domain,
        mintRecipient: word(burn.mint_recipient),
        burnToken: address(burn.burn_token),
        destinationCaller: word(burn.destination_caller),
        maxFee: amount(burn.max_fee),
        minFinalityThreshold: burn.min_finality_threshold,
    }
    .abi_encode()
}

/// `receiveMessage(bytes,bytes)`.
pub fn cctp_receive_message(message: &[u8], attestation: &[u8]) -> Vec<u8> {
    receiveMessageCall {
        message: message.to_vec().into(),
        attestation: attestation.to_vec().into(),
    }
    .abi_encode()
}

/// `publishAndFund(uint64,bytes,(uint64,address,address,uint256,(address,uint256)[]),bool)`:
/// publishes the intent whose route is `route` for `destination` and funds its reward
/// from the caller, never partially. `destination` is the chain Eco's own quote named,
/// which is not always the chain the user is paid on.
pub fn eco_publish_and_fund(destination: ChainId, route: &[u8], reward: &EcoReward) -> Vec<u8> {
    publishAndFundCall {
        destination: destination.get(),
        route: route.to_vec().into(),
        reward: self::reward(reward),
        allowPartial: false,
    }
    .abi_encode()
}

/// The hash the Portal knows a route by: the keccak of its bytes.
pub fn eco_route_hash(route: &[u8]) -> [u8; 32] {
    keccak256(route).0
}

/// `refund(uint64,bytes32,(uint64,address,address,uint256,(address,uint256)[]))`:
/// permissionless after the reward's deadline, and pays the reward back to its creator.
pub fn eco_refund(destination: ChainId, route_hash: [u8; 32], reward: &EcoReward) -> Vec<u8> {
    ecoRefundCall {
        destination: destination.get(),
        routeHash: word(route_hash),
        reward: self::reward(reward),
    }
    .abi_encode()
}

/// What a vault execution says: the swap it is for, the calls it runs and the floors the
/// vault holds them to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultExecution {
    pub swap_ref: QuoteHash,
    pub calls: Vec<VaultCall>,
    pub deltas: Vec<VaultDelta>,
}

/// What a vault transfer says: the swap it closes, the token, who is paid and how much.
/// `payout` and `refund` are the same shape and differ only in which side of a swap they
/// end, so one type reads both back and no caller has to remember which address is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultTransfer {
    pub swap_ref: QuoteHash,
    pub token: EvmAddress,
    pub to: EvmAddress,
    pub amount: TokenAmount,
}

/// What a Permit2 pull says: the quote it is witnessed by, whose funds it moves, the
/// permit they signed and their signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permit2Pull {
    pub quote_hash: QuoteHash,
    pub owner: EvmAddress,
    pub permit: Permit2Permit,
    pub signature: Vec<u8>,
}

/// What an EIP-2612 pull says. This canister sends none (see
/// [`vault_pull_with_permit`]); the decoder is here so a test can read one back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Eip2612Pull {
    pub quote_hash: QuoteHash,
    pub token: EvmAddress,
    pub owner: EvmAddress,
    pub amount: TokenAmount,
    pub deadline: UnixSeconds,
    pub signature: Permit,
}

/// What a CCTP mint carries: Circle's message and its attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CctpMint {
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
}

/// What an Eco publish says: the destination Eco named, the route a filler runs and the
/// reward the vault locks for them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcoPublish {
    pub destination: ChainId,
    pub route: Vec<u8>,
    pub reward: EcoReward,
}

/// What an Eco reclaim says: the intent it names, by destination and route hash, and the
/// reward that goes back to its creator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcoReclaim {
    pub destination: ChainId,
    pub route_hash: [u8; 32],
    pub reward: EcoReward,
}

/// The execute `data` encodes, if it is one.
pub fn decode_vault_execute(data: &[u8]) -> Option<VaultExecution> {
    let call = executeCall::abi_decode(data).ok()?;
    let calls = call
        .calls
        .into_iter()
        .map(|call| VaultCall {
            target: evm_address(call.target),
            value: checked(call.value),
            data: call.data.to_vec(),
            approve_token: evm_address(call.approveToken),
            approve_amount: checked(call.approveAmount),
        })
        .collect();
    let deltas = call
        .deltas
        .into_iter()
        .map(|delta| {
            Some(VaultDelta {
                token: evm_address(delta.token),
                min_change: i128::try_from(delta.minChange).ok()?,
            })
        })
        .collect::<Option<_>>()?;
    Some(VaultExecution {
        swap_ref: QuoteHash::new(call.swapRef.0),
        calls,
        deltas,
    })
}

/// The payout `data` encodes, if it is one.
pub fn decode_vault_payout(data: &[u8]) -> Option<VaultTransfer> {
    let call = payoutCall::abi_decode(data).ok()?;
    Some(VaultTransfer {
        swap_ref: QuoteHash::new(call.swapRef.0),
        token: evm_address(call.token),
        to: evm_address(call.to),
        amount: checked(call.amount),
    })
}

/// The refund `data` encodes, if it is one.
pub fn decode_vault_refund(data: &[u8]) -> Option<VaultTransfer> {
    let call = refundCall::abi_decode(data).ok()?;
    Some(VaultTransfer {
        swap_ref: QuoteHash::new(call.r#ref.0),
        token: evm_address(call.token),
        to: evm_address(call.to),
        amount: checked(call.amount),
    })
}

/// The Permit2 pull `data` encodes, if it is one.
pub fn decode_vault_pull_with_permit2(data: &[u8]) -> Option<Permit2Pull> {
    let call = pullWithPermit2Call::abi_decode(data).ok()?;
    Some(Permit2Pull {
        quote_hash: QuoteHash::new(call.quoteHash.0),
        owner: evm_address(call.owner),
        permit: Permit2Permit {
            token: evm_address(call.permit.permitted.token),
            amount: checked(call.permit.permitted.amount),
            nonce: checked(call.permit.nonce),
            deadline: UnixSeconds::new(u64::try_from(call.permit.deadline).ok()?),
        },
        signature: call.signature.to_vec(),
    })
}

/// The EIP-2612 pull `data` encodes, if it is one. No path in this canister builds one;
/// the decoder answers the call's own shape so the ABI stays readable from both ends.
pub fn decode_vault_pull_with_permit(data: &[u8]) -> Option<Eip2612Pull> {
    let call = pullWithPermitCall::abi_decode(data).ok()?;
    Some(Eip2612Pull {
        quote_hash: QuoteHash::new(call.quoteHash.0),
        token: evm_address(call.token),
        owner: evm_address(call.owner),
        amount: checked(call.amount),
        deadline: UnixSeconds::new(u64::try_from(call.deadline).ok()?),
        signature: Permit {
            v: call.v,
            r: call.r.0,
            s: call.s.0,
        },
    })
}

/// The burn `data` encodes, if it is one.
pub fn decode_cctp_deposit_for_burn(data: &[u8]) -> Option<Burn> {
    let call = depositForBurnCall::abi_decode(data).ok()?;
    Some(Burn {
        amount: checked(call.amount),
        destination_domain: call.destinationDomain,
        mint_recipient: call.mintRecipient.0,
        burn_token: evm_address(call.burnToken),
        destination_caller: call.destinationCaller.0,
        max_fee: checked(call.maxFee),
        min_finality_threshold: call.minFinalityThreshold,
    })
}

/// The message and attestation `data` carries, if it is a `receiveMessage`.
pub fn decode_cctp_receive_message(data: &[u8]) -> Option<CctpMint> {
    let call = receiveMessageCall::abi_decode(data).ok()?;
    Some(CctpMint {
        message: call.message.to_vec(),
        attestation: call.attestation.to_vec(),
    })
}

/// The intent `data` publishes, if it is a `publishAndFund`.
pub fn decode_eco_publish_and_fund(data: &[u8]) -> Option<EcoPublish> {
    let call = publishAndFundCall::abi_decode(data).ok()?;
    Some(EcoPublish {
        destination: ChainId::new(call.destination),
        route: call.route.to_vec(),
        reward: eco_reward(call.reward),
    })
}

/// The reclaim `data` asks for, if it is the Portal's `refund`.
pub fn decode_eco_refund(data: &[u8]) -> Option<EcoReclaim> {
    let call = ecoRefundCall::abi_decode(data).ok()?;
    Some(EcoReclaim {
        destination: ChainId::new(call.destination),
        route_hash: call.routeHash.0,
        reward: eco_reward(call.reward),
    })
}
