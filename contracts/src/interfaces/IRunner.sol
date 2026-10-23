// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Order, Recipe} from "../SwapicTypes.sol";
import {Envelope} from "../libraries/Envelope.sol";

/// Runs the signed steps of an order (only Swapic may ask) and every arrival
/// (anyone may ask, paying with their own tokens). Nobody approves it, it
/// holds nothing between transactions, and every outcome is all or nothing.
interface IRunner {
    /// A same-chain order paid `amount` of `token` to `recipient`.
    event PaidOut(bytes32 indexed id, address indexed recipient, address token, uint256 amount);
    /// An arrival of `amountIn` was swapped whole into `amountOut` of `tokenOut`.
    event Delivered(
        bytes32 indexed ref, address indexed recipient, address tokenOut, uint256 amountIn, uint256 amountOut
    );
    event CkDeposited(bytes32 indexed ref, bytes32 indexed principal, bytes32 subaccount, uint256 amount);
    /// The arrival action failed: all of the arrived `token` went to `recipient`.
    event Refunded(bytes32 indexed ref, address indexed recipient, address token, uint256 amount);

    error OnlySwapic();
    error OnlySelf();
    error Reentrancy();
    error IsPaused();
    error BadStep();
    error BadWrite();
    error Short();
    error LeftOver();
    error CallFailed();
    error WrongAction();
    error BadArrival();
    error BadRecipient();
    error NoHelper();
    error OutOfGas();

    function swapic() external view returns (address);
    function cctpReceiver() external view returns (address);
    function ckHelper() external view returns (address);
    function wrappedNative() external view returns (address);

    /// Only Swapic, after it pulled `amountIn` into the Runner. `inBaseline`
    /// is the Runner's `tokenIn` balance before that pull (for a NATIVE
    /// tokenIn: of the wrapped native token, which is what arrived).
    function execute(Order calldata order, bytes32 id, uint256 inBaseline) external;

    /// Anyone (Eco's executor, the CCTP receiver, any rail that can approve
    /// and call): pulls `amount` of `token` from the caller, then acts on
    /// exactly what arrived as the envelope says. Swap: through `recipe`, all
    /// of it into at least `minOut` of `tokenOut`, all paid to the recipient;
    /// CkDeposit: all of it into the ck helper. If the action fails, all of
    /// what arrived goes to the recipient instead. Reverts (so nothing
    /// arrives) when paused, while a Swapic door runs, on a bad or Deliver
    /// envelope or a bad recipient, when less than the envelope's `minGas`
    /// is left for the action or the action was starved of gas (`OutOfGas`,
    /// retry with more), or if the refund transfer itself fails.
    function fill(address token, uint256 amount, bytes calldata envelope, Recipe calldata recipe) external;

    /// Self-call only: the undoable halves of `fill`.
    function swapAll(address token, uint256 received, uint256 base, Envelope.Data calldata e, Recipe calldata recipe)
        external
        returns (uint256 out);
    function ckDepositAll(address token, uint256 received, uint256 base, Envelope.Data calldata e) external;
}
