// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// The rail a swap crosses on.
enum Rail {
    Cctp,
    Eco
}

/// What happens on the destination once the stable lands.
///   - Deliver: the rail pays the recipient directly (CCTP mints to it, the
///     Eco route transfers to it). The executor is not involved.
///   - Swap: the executor swaps the stable into `tokenOut` for the recipient,
///     at least `minOut`, or pays the recipient the stable.
///   - CkDeposit: on Ethereum, the executor deposits the stable into the ck
///     minter's helper for `principal` and `subaccount`, or pays the
///     recipient (the user's Ethereum fallback address) the stable.
enum Action {
    Deliver,
    Swap,
    CkDeposit
}

/// The gasless pull doors, in the order of preference the quoter applies.
enum Door {
    Auth3009,
    Permit2612,
    Permit2
}

/// One call into an allowlisted, stateless DEX router. The router approves
/// exactly the input amount to `target`, calls it with `data`, and resets the
/// approval to zero. A zero `target` means no call. The calldata must pay its
/// output to the caller (the router or the executor), never elsewhere.
struct DexCall {
    address target;
    bytes data;
}

/// The fields of an Eco intent that Eco's own quote fixes. The router builds
/// the rest of the intent from the registry and the other params.
struct EcoTerms {
    /// the route salt from Eco's quote
    bytes32 salt;
    /// fills stop at this second on the destination
    uint64 routeDeadline;
    /// the permissionless refund to the payer opens at this second
    uint64 rewardDeadline;
    /// the reward: exactly what the router bridges (Eco's quoted source amount)
    uint256 amount;
    /// the route's token amount on the destination (what the solver delivers)
    uint256 amountOut;
}

/// The destination side of a quote.
struct Delivery {
    Action action;
    /// The user on the destination. Deliver: receives the stable (on CCTP it
    /// may be any bytes32 mint recipient, e.g. a Solana token account).
    /// Swap: receives `tokenOut`, or the stable on a fallback. CkDeposit: the
    /// user's Ethereum fallback address. Must be an EVM address (high 12 bytes
    /// zero) for every action except a CCTP Deliver.
    bytes32 recipient;
    /// Swap only: the token the user receives
    address tokenOut;
    /// The absolute floor for the user: in `tokenOut` units on a Swap, in the
    /// destination stable's units on Deliver and CkDeposit.
    uint256 minOut;
    /// CkDeposit only: the ICP principal, as the ck helper takes it
    bytes32 principal;
    /// CkDeposit only: the ICRC-1 subaccount
    bytes32 subaccount;
    /// End of the keeper-or-user window on the destination executor. After it
    /// only the permissionless `release` exists (CCTP).
    uint64 deadline;
    /// Eco Swap only: the destination swap, fixed into the route at quote time.
    /// CCTP destination swaps take fresh swap data from the keeper instead.
    DexCall swap;
}

/// Everything a quote fixes, and all of it is hashed into the quote hash.
struct SwapParams {
    /// Quoter-chosen random value, known before any provider quote. It makes
    /// every quote unique and keys the Eco destination leg (an Eco route cannot
    /// contain the quote hash, see abi.md).
    bytes32 ref;
    /// Whose funds. `swap` requires payer == msg.sender. Also the Eco reward's
    /// creator (so every Eco refund pays the payer) and the receiver of any
    /// excess stable and unspent input.
    address payer;
    address tokenIn;
    uint256 amountIn;
    /// The source transaction must land at or before this second.
    uint256 deadline;
    /// The optional source DEX leg, tokenIn to the stable. Zero target: no leg,
    /// and tokenIn must be the stable.
    DexCall leg;
    /// The stable the source side must produce at least (after the leg, before
    /// fees). With no leg it bounds the measured input.
    uint256 legMinOut;
    /// The platform fee in basis points; must equal the router's current knob.
    uint256 feeBps;
    /// Gasless only: the cap, in the stable, of the relayer's gas reimbursement.
    uint256 maxGasFee;
    Rail rail;
    uint64 dstChainId;
    /// CCTP only: the burn's max fee, in the stable.
    uint256 maxFee;
    /// CCTP only: 1000 (fast) or 2000 (standard).
    uint32 minFinality;
    /// Eco only.
    EcoTerms eco;
    Delivery dst;
}

/// The gasless authorization `swapFor` takes beside the params. Only
/// `gasFee` is chosen by the relayer, and it is capped by `maxGasFee`, which
/// the payer signed through the quote hash.
struct GaslessAuth {
    Door door;
    /// the reimbursement the relayer takes, in the stable, at most maxGasFee
    uint256 gasFee;
    /// Auth3009: the authorization's window (both bounds strict on USDC)
    uint256 validAfter;
    uint256 validBefore;
    /// Permit2: the signature transfer's nonce and deadline
    uint256 nonce;
    uint256 deadline;
    /// Auth3009: r ++ s ++ v (65 bytes). Permit2612: the permit's r ++ s ++ v
    /// (65 bytes; swallowed if it fails). Permit2: the witness signature.
    bytes signature;
    /// Permit2612 only: the payer's signature over the QuoteAcceptance.
    bytes acceptance;
}

/// A user-direct Permit2 signature transfer (owner = msg.sender).
struct Permit2Auth {
    uint256 nonce;
    uint256 deadline;
    bytes signature;
}
