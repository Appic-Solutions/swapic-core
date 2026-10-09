// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "../../src/interfaces/IEcoPortal.sol";

/// CCTP v2's TokenMessengerV2 in miniature: it pulls the burn from the caller
/// and records the call, with the two refusals the router relies on.
contract CctpMessengerMock {
    struct Burn {
        uint256 amount;
        uint32 domain;
        bytes32 mintRecipient;
        address burnToken;
        bytes32 destinationCaller;
        uint256 maxFee;
        uint32 minFinality;
        bytes hookData;
    }

    Burn public last;

    function depositForBurnWithHook(
        uint256 amount,
        uint32 domain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinality,
        bytes calldata hookData
    ) external {
        require(hookData.length > 0, "Hook data is empty");
        require(maxFee < amount, "Max fee must be less than amount");
        IERC20(burnToken).transferFrom(msg.sender, address(0xdead), amount);
        last = Burn(amount, domain, mintRecipient, burnToken, destinationCaller, maxFee, minFinality, hookData);
    }

    function lastHookData() external view returns (bytes memory) {
        return last.hookData;
    }
}

/// Eco's Portal publish in miniature, with Eco's own hashing and its funding
/// rule: it takes min(remaining, allowance, balance) from the funder.
contract EcoPortalV2Mock {
    bytes public lastRoute;

    function publishAndFund(uint64 destination, bytes calldata route, IEcoPortal.Reward calldata reward, bool)
        external
        payable
        returns (bytes32 intentHash, address vault)
    {
        intentHash = keccak256(abi.encodePacked(destination, keccak256(route), keccak256(abi.encode(reward))));
        lastRoute = route;
        for (uint256 i = 0; i < reward.tokens.length; i++) {
            IERC20 t = IERC20(reward.tokens[i].token);
            uint256 a = reward.tokens[i].amount;
            uint256 allow = t.allowance(msg.sender, address(this));
            uint256 bal = t.balanceOf(msg.sender);
            uint256 take = a < allow ? a : allow;
            take = take < bal ? take : bal;
            require(take == a, "InsufficientFunds");
            t.transferFrom(msg.sender, address(this), take);
        }
        vault = address(this);
    }
}

/// A DEX router that pays `rate` per unit, to the caller, from its own float.
contract DexMock {
    function swap(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(msg.sender, amountOut);
    }

    /// pays the output to someone else: the measured delta must catch it
    function swapTo(address tokenIn, address tokenOut, uint256 amountIn, uint256 amountOut, address to) external {
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        IERC20(tokenOut).transfer(to, amountOut);
    }
}
