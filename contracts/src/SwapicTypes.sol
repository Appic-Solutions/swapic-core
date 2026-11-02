// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

// Ether, wherever a token address is expected.
address constant NATIVE = 0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE;

// Uniswap's Permit2: a door for Swapic, a banned target for the Runner.
address constant PERMIT2 = 0x000000000022D473030F116dDEE9F6B43aC78BA3;

/// Writes (delta - minus) as one uint256 word at byte `at` of a step's data.
/// `at` is any byte offset, not a word index.
struct Write {
    uint32 at;
    uint256 minus;
}

/// One call the Runner makes. `token` ERC-20: the Runner approves `target`
/// exactly its delta of `token`; `token == NATIVE`: it sends that delta as
/// value; `token == 0`: no approval, no value, no writes.
struct Step {
    address target;
    bytes data;
    address token;
    uint256 min;
    Write[] writes;
}

/// `token == 0`: no payout (cross-chain orders).
struct Payout {
    address token;
    address recipient;
    uint256 minOut;
}

/// `memo`: a pool deposit's encoded terms, whose keccak256 is `ref`; empty on
/// every other order. Swapic only signs it into the id and emits it.
struct Order {
    bytes32 ref;
    address payer;
    address tokenIn;
    uint256 amountIn;
    uint256 deadline;
    Step[] steps;
    Payout payout;
    bytes memo;
}

enum Door {
    Auth3009,
    Permit2612,
    Permit2
}

/// Auth3009: `validAfter`, `validBefore` and `signature` (r ++ s ++ v).
/// Permit2612: `signature` is the permit (swallowed if it fails) and
/// `acceptance` the payer's QuoteAcceptance signature. Permit2: `nonce`,
/// `deadline` and `signature` of the witness transfer.
struct GaslessAuth {
    Door door;
    uint256 validAfter;
    uint256 validBefore;
    uint256 nonce;
    uint256 deadline;
    bytes signature;
    bytes acceptance;
}

struct Permit2Auth {
    uint256 nonce;
    uint256 deadline;
    bytes signature;
}

enum Action {
    Deliver,
    Swap,
    CkDeposit
}

/// The arrival swap. `scaler == 0`: `data` as is; else `data` is first
/// rescaled to the amount that arrived.
struct Recipe {
    address target;
    bytes data;
    address scaler;
}
