// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "./helpers/EcoPlan.sol";
import {PORTAL} from "./helpers/TestConstants.sol";

/// The Eco offsets of abi.md section 6, checked against the encoder: the
/// step built with zero amounts, with the Runner's writes applied for a
/// delta X, must equal the call encoded directly at X and X - fee, for route
/// lengths that land on and off a word boundary.
contract EcoPlanTest is Test {
    function _terms() internal pure returns (EcoPlan.Terms memory t) {
        t.destination = 42161;
        t.portal = PORTAL;
        t.dstToken = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
        t.srcToken = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
        t.salt = keccak256("salt");
        t.routeDeadline = 1_792_069_649;
        t.rewardDeadline = 1_792_674_449;
        t.creator = address(0xA11CE);
        t.prover = 0xEC08fb4647f3f50d1162a578d481266687C60fc5;
        t.fee = 58_731;
    }

    /// The Runner's write, verbatim in effect.
    function _apply(bytes memory data, Write[] memory ws, uint256 d) internal pure returns (bytes memory) {
        for (uint256 i; i < ws.length; ++i) {
            uint256 at = ws[i].at;
            uint256 v = d - ws[i].minus;
            require(at + 32 <= data.length, "oob");
            assembly {
                mstore(add(add(data, 32), at), v)
            }
        }
        return data;
    }

    function test_swap_offsets() public pure {
        EcoPlan.Terms memory t = _terms();
        uint256 x = 5_013_742;
        uint256[6] memory lens = [uint256(0), 1, 31, 32, 33, 3236];
        for (uint256 i; i < lens.length; ++i) {
            Recipe memory recipe = Recipe(address(0xD3C), new bytes(lens[i]), address(0x5CA1E));
            bytes memory env = new bytes(320);
            env[287] = 0x01;
            Step memory s = EcoPlan.swapStep(t, PORTAL, address(0x12), env, recipe, 0);
            bytes memory want = abi.encodeCall(
                IEcoPortal.publishAndFund,
                (
                    t.destination,
                    EcoPlan.route(t, EcoPlan.swapCalls(t, address(0x12), x - t.fee, env, recipe), x - t.fee),
                    EcoPlan.reward(t, x),
                    false
                )
            );
            assertEq(_apply(s.data, s.writes, x), want);
        }
    }

    function test_deliver_offsets() public pure {
        EcoPlan.Terms memory t = _terms();
        uint256 x = 5_013_742;
        Step memory s = EcoPlan.deliverStep(t, PORTAL, address(0xA11CE), 0);
        bytes memory want = abi.encodeCall(
            IEcoPortal.publishAndFund,
            (
                t.destination,
                EcoPlan.route(t, EcoPlan.deliverCalls(t, address(0xA11CE), x - t.fee), x - t.fee),
                EcoPlan.reward(t, x),
                false
            )
        );
        assertEq(_apply(s.data, s.writes, x), want);
        assertEq(s.writes.length, 3);
    }
}
