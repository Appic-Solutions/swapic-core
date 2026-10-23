// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Recipe} from "../SwapicTypes.sol";
import {Envelope} from "../libraries/Envelope.sol";

/// The only CCTP code: the `mintRecipient` and `destinationCaller` of every
/// CCTP Swap and CkDeposit burn. It claims the message and hands exactly what
/// was minted to `Runner.fill`, or after the deadline to the recipient.
interface ICctpReceiver {
    /// After the deadline: the minted USDC went to the recipient.
    event Released(bytes32 indexed ref, address indexed recipient, uint256 amount);

    error IsPaused();
    error Reentrancy();
    error BadMessage();
    error WrongAction();
    error BadRecipe();
    error DeadlinePassed();
    error DeadlineNotPassed();
    error NotCanisterOrRecipient();
    error LeftOver();
    error NothingMinted();

    function swapic() external view returns (address);
    function usdc() external view returns (address);
    function transmitter() external view returns (address);
    function tokenMessenger() external view returns (address);

    /// Before the envelope's deadline, by the canister or the envelope's
    /// recipient: receives the message, then `Runner.fill` on exactly what
    /// was minted with `recipe` (empty for CkDeposit). The message must be a
    /// burn for the local TokenMessenger, with this contract as destination
    /// caller and mint recipient, and mint something.
    function completeCctp(bytes calldata message, bytes calldata attestation, Recipe calldata recipe) external;

    /// After the envelope's deadline, by anyone, even paused: receives the
    /// message and pays all that was minted to the envelope's recipient.
    function release(bytes calldata message, bytes calldata attestation) external;

    /// The envelope of any Swapic-sized CCTP v2 message (bytes 376 to 696),
    /// Deliver included. It does not say the receiver will act on it.
    function envelopeOf(bytes calldata message) external pure returns (Envelope.Data memory);

    /// The envelope of a message `completeCctp` and `release` will act on
    /// (same header and mint recipient checks); reverts otherwise.
    function claimable(bytes calldata message) external view returns (Envelope.Data memory);
}
