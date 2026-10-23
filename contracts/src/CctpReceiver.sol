// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Recipe, Action} from "./SwapicTypes.sol";
import {ICctpReceiver} from "./interfaces/ICctpReceiver.sol";
import {ISwapic} from "./interfaces/ISwapic.sol";
import {IRunner} from "./interfaces/IRunner.sol";
import {IMessageTransmitterV2} from "./interfaces/ICctpV2.sol";
import {Envelope} from "./libraries/Envelope.sol";

/// Holds nothing between transactions: every mint it claims leaves in the
/// same call, to the Runner or to the recipient.
contract CctpReceiver is ICctpReceiver {
    using SafeERC20 for IERC20;

    /// CCTP v2 message layout (Circle evm-cctp-contracts): the header's
    /// recipient at 76 and destination caller at 108, the body at 148, the
    /// burn message's mint recipient at body + 36, its hook data at body + 228.
    uint256 internal constant RECIPIENT_AT = 76;
    uint256 internal constant CALLER_AT = 108;
    uint256 internal constant MINT_RECIPIENT_AT = 148 + 36;
    uint256 internal constant ENVELOPE_AT = 148 + 228;
    uint256 internal constant LENGTH = ENVELOPE_AT + Envelope.LENGTH;

    address public immutable swapic;
    address public immutable usdc;
    address public immutable transmitter;
    address public immutable tokenMessenger;

    uint256 private _lock = 1;

    modifier nonReentrant() {
        if (_lock != 1) revert Reentrancy();
        _lock = 2;
        _;
        _lock = 1;
    }

    constructor(address swapic_, address usdc_, address transmitter_, address tokenMessenger_) {
        swapic = swapic_;
        usdc = usdc_;
        transmitter = transmitter_;
        tokenMessenger = tokenMessenger_;
    }

    function completeCctp(bytes calldata message, bytes calldata attestation, Recipe calldata recipe)
        external
        nonReentrant
    {
        ISwapic s = ISwapic(swapic);
        if (s.paused()) revert IsPaused();
        Envelope.Data memory e = _envelope(message);
        if (e.action == Action.Deliver) revert WrongAction();
        if (
            e.action == Action.CkDeposit
                && (recipe.target != address(0) || recipe.data.length != 0 || recipe.scaler != address(0))
        ) revert BadRecipe();
        if (block.timestamp > e.deadline) revert DeadlinePassed();
        if (msg.sender != s.canister() && msg.sender != e.recipient) revert NotCanisterOrRecipient();

        IERC20 token = IERC20(usdc);
        uint256 base = token.balanceOf(address(this));
        uint256 minted = _receive(message, attestation, base);
        address runner = s.runner();
        token.forceApprove(runner, minted);
        IRunner(runner).fill(usdc, minted, message[ENVELOPE_AT:], recipe);
        token.forceApprove(runner, 0);
        if (token.balanceOf(address(this)) != base) revert LeftOver();
    }

    function release(bytes calldata message, bytes calldata attestation) external nonReentrant {
        Envelope.Data memory e = _envelope(message);
        if (block.timestamp <= e.deadline) revert DeadlineNotPassed();
        uint256 minted = _receive(message, attestation, IERC20(usdc).balanceOf(address(this)));
        IERC20(usdc).safeTransfer(e.recipient, minted);
        emit Released(e.ref, e.recipient, minted);
    }

    /// Decodes the envelope of any Swapic-sized message, Deliver included. It
    /// does not say the receiver will act on it: `claimable` does.
    function envelopeOf(bytes calldata message) external pure returns (Envelope.Data memory) {
        if (message.length != LENGTH) revert BadMessage();
        return Envelope.decode(message[ENVELOPE_AT:]);
    }

    /// The envelope of a message `completeCctp` and `release` will act on, under
    /// the same header and mint recipient checks; reverts otherwise.
    function claimable(bytes calldata message) external view returns (Envelope.Data memory) {
        return _envelope(message);
    }

    /// The envelope of a Circle burn message that only this contract may
    /// receive and that mints to it. Pinning the header recipient to the
    /// TokenMessenger keeps any other message handler (code that runs between
    /// the baseline and the mint) out.
    function _envelope(bytes calldata message) internal view returns (Envelope.Data memory) {
        if (message.length != LENGTH) revert BadMessage();
        bytes32 me = bytes32(uint256(uint160(address(this))));
        if (
            bytes32(message[RECIPIENT_AT:RECIPIENT_AT + 32]) != bytes32(uint256(uint160(tokenMessenger)))
                || bytes32(message[CALLER_AT:CALLER_AT + 32]) != me
                || bytes32(message[MINT_RECIPIENT_AT:MINT_RECIPIENT_AT + 32]) != me
        ) revert BadMessage();
        return Envelope.decode(message[ENVELOPE_AT:]);
    }

    /// What the transmitter minted here in this call.
    function _receive(bytes calldata message, bytes calldata attestation, uint256 base)
        internal
        returns (uint256 minted)
    {
        if (!IMessageTransmitterV2(transmitter).receiveMessage(message, attestation)) revert BadMessage();
        minted = IERC20(usdc).balanceOf(address(this)) - base;
        if (minted == 0) revert NothingMinted();
    }
}
