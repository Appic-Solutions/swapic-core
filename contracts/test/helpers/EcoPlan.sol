// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Step, Write, Recipe} from "../../src/SwapicTypes.sol";
import {IRunner} from "../../src/interfaces/IRunner.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IEcoPortal} from "./IEcoPortal.sol";

/// What the quoter writes for Eco: one `publishAndFund` step whose amounts the
/// Runner writes in at run time. The reward is the whole delta X; the route
/// token amount and the call amounts are X - fee. Offsets follow from the ABI
/// layout of `publishAndFund(uint64,bytes,Reward,bool)` (abi.md section 6):
///
///   the route bytes start at 4 + 0x80 + 0x20 = 0xa4 (selector, 4 head words,
///   the bytes length word); inside them (abi.encode(Route): a leading 0x20,
///   the 6 word Route head, one token, the calls):
///     route token amount   0x120
///     Swap:    approve amount 0x244, fill amount 0x324 (behind 4 byte selectors)
///     Deliver: transfer amount 0x224
///   the reward amount at 0xa4 + ceil32(route length) + 0xe0.
library EcoPlan {
    uint32 internal constant ROUTE_AT = 0xa4;
    uint32 internal constant TOKEN_AMOUNT = 0x120;
    uint32 internal constant SWAP_APPROVE_AMOUNT = 0x244;
    uint32 internal constant SWAP_FILL_AMOUNT = 0x324;
    uint32 internal constant DELIVER_TRANSFER_AMOUNT = 0x224;

    struct Terms {
        uint64 destination;
        address portal;
        address dstToken;
        address srcToken;
        bytes32 salt;
        uint64 routeDeadline;
        uint64 rewardDeadline;
        address creator;
        address prover;
        uint256 fee;
    }

    function swapCalls(Terms memory t, address runner, uint256 amount, bytes memory envelope, Recipe memory recipe)
        internal
        pure
        returns (IEcoPortal.Call[] memory calls)
    {
        calls = new IEcoPortal.Call[](2);
        calls[0] = IEcoPortal.Call(t.dstToken, abi.encodeCall(IERC20.approve, (runner, amount)), 0);
        calls[1] = IEcoPortal.Call(runner, abi.encodeCall(IRunner.fill, (t.dstToken, amount, envelope, recipe)), 0);
    }

    function deliverCalls(Terms memory t, address recipient, uint256 amount)
        internal
        pure
        returns (IEcoPortal.Call[] memory calls)
    {
        calls = new IEcoPortal.Call[](1);
        calls[0] = IEcoPortal.Call(t.dstToken, abi.encodeCall(IERC20.transfer, (recipient, amount)), 0);
    }

    function route(Terms memory t, IEcoPortal.Call[] memory calls, uint256 amount) internal pure returns (bytes memory) {
        IEcoPortal.Route memory r;
        r.salt = t.salt;
        r.deadline = t.routeDeadline;
        r.portal = t.portal;
        r.tokens = new IEcoPortal.TokenAmount[](1);
        r.tokens[0] = IEcoPortal.TokenAmount(t.dstToken, amount);
        r.calls = calls;
        return abi.encode(r);
    }

    function reward(Terms memory t, uint256 amount) internal pure returns (IEcoPortal.Reward memory r) {
        r.deadline = t.rewardDeadline;
        r.creator = t.creator;
        r.prover = t.prover;
        r.tokens = new IEcoPortal.TokenAmount[](1);
        r.tokens[0] = IEcoPortal.TokenAmount(t.srcToken, amount);
    }

    function intentHash(Terms memory t, bytes memory routeBytes, IEcoPortal.Reward memory r)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(t.destination, keccak256(routeBytes), keccak256(abi.encode(r))));
    }

    function rewardAt(uint256 routeLength) internal pure returns (uint32) {
        return uint32(ROUTE_AT + (routeLength + 31) / 32 * 32 + 0xe0);
    }

    function _step(Terms memory t, address sourcePortal, bytes memory routeBytes, uint256 min, Write[] memory ws)
        private
        pure
        returns (Step memory)
    {
        ws[0] = Write(rewardAt(routeBytes.length), 0);
        ws[1] = Write(ROUTE_AT + TOKEN_AMOUNT, t.fee);
        return Step(
            sourcePortal,
            abi.encodeCall(IEcoPortal.publishAndFund, (t.destination, routeBytes, reward(t, 0), false)),
            t.srcToken,
            min,
            ws
        );
    }

    /// Route calls [usdc.approve(Runner, X - fee), Runner.fill(usdc, X - fee, envelope, recipe)].
    function swapStep(
        Terms memory t,
        address sourcePortal,
        address runner,
        bytes memory envelope,
        Recipe memory recipe,
        uint256 min
    ) internal pure returns (Step memory) {
        Write[] memory ws = new Write[](4);
        ws[2] = Write(ROUTE_AT + SWAP_APPROVE_AMOUNT, t.fee);
        ws[3] = Write(ROUTE_AT + SWAP_FILL_AMOUNT, t.fee);
        return _step(t, sourcePortal, route(t, swapCalls(t, runner, 0, envelope, recipe), 0), min, ws);
    }

    /// Route calls [usdc.transfer(recipient, X - fee)].
    function deliverStep(Terms memory t, address sourcePortal, address recipient, uint256 min)
        internal
        pure
        returns (Step memory)
    {
        Write[] memory ws = new Write[](3);
        ws[2] = Write(ROUTE_AT + DELIVER_TRANSFER_AMOUNT, t.fee);
        return _step(t, sourcePortal, route(t, deliverCalls(t, recipient, 0), 0), min, ws);
    }
}
