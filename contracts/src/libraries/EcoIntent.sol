// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SwapParams, Action} from "../SwapicTypes.sol";
import {IEcoPortal} from "../interfaces/IEcoPortal.sol";
import {ISwapicExecutor} from "../interfaces/ISwapicExecutor.sol";

/// The router's fixed Eco intent template (spec 4b.2). The router composes
/// the whole intent from the signed params and its per-chain registry and
/// never takes route bytes from anyone:
///
///   Route  { salt: eco.salt, deadline: eco.routeDeadline, portal: dst portal,
///            nativeAmount: 0, tokens: [(dst stable, eco.amountOut)], calls }
///   calls, Deliver:          [ dst stable.transfer(recipient, amountOut) ]
///   calls, Swap, CkDeposit:  [ dst stable.approve(dst executor, amountOut),
///                              dst executor.fill(amountOut, hookData, dst.swap) ]
///   Reward { deadline: eco.rewardDeadline, creator: payer, prover: the
///            registry prover, nativeAmount: 0, tokens: [(src stable, eco.amount)] }
///   intentHash = keccak256(abi.encodePacked(uint64 dstChainId,
///                          keccak256(abi.encode(route)), keccak256(abi.encode(reward))))
///
/// The creator is the payer, so every permissionless exit of the reward
/// (refund, recover) pays the payer and never the router. The executor pulls
/// the approved amount inside `fill` itself, so it acts only on what it
/// received in that call (4b.6), and nothing is left in Eco's executor.
library EcoIntent {
    struct Destination {
        address portal;
        address stable;
        address executor;
    }

    function compose(
        SwapParams memory p,
        Destination memory d,
        address prover,
        address srcStable,
        bytes memory hookData
    ) internal pure returns (bytes memory route, IEcoPortal.Reward memory reward, bytes32 intentHash) {
        IEcoPortal.Route memory r;
        r.salt = p.eco.salt;
        r.deadline = p.eco.routeDeadline;
        r.portal = d.portal;
        r.tokens = new IEcoPortal.TokenAmount[](1);
        r.tokens[0] = IEcoPortal.TokenAmount(d.stable, p.eco.amountOut);
        if (p.dst.action == Action.Deliver) {
            r.calls = new IEcoPortal.Call[](1);
            address to = address(uint160(uint256(p.dst.recipient)));
            r.calls[0] = IEcoPortal.Call(d.stable, abi.encodeCall(IERC20.transfer, (to, p.eco.amountOut)), 0);
        } else {
            r.calls = new IEcoPortal.Call[](2);
            r.calls[0] = IEcoPortal.Call(d.stable, abi.encodeCall(IERC20.approve, (d.executor, p.eco.amountOut)), 0);
            r.calls[1] = IEcoPortal.Call(
                d.executor, abi.encodeCall(ISwapicExecutor.fill, (p.eco.amountOut, hookData, p.dst.swap)), 0
            );
        }
        route = abi.encode(r);

        reward.deadline = p.eco.rewardDeadline;
        reward.creator = p.payer;
        reward.prover = prover;
        reward.tokens = new IEcoPortal.TokenAmount[](1);
        reward.tokens[0] = IEcoPortal.TokenAmount(srcStable, p.eco.amount);

        intentHash = keccak256(abi.encodePacked(p.dstChainId, keccak256(route), keccak256(abi.encode(reward))));
    }
}
