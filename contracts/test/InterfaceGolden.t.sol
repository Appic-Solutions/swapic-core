// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicTypes.sol";
import {OrderHash} from "../src/libraries/OrderHash.sol";
import {ISwapic} from "../src/interfaces/ISwapic.sol";
import {Swapic} from "../src/Swapic.sol";

contract GoldenHarness {
    function orderId(Order calldata o, uint256 chainId, address swapic) external pure returns (bytes32) {
        return OrderHash.orderId(o, chainId, swapic);
    }

    function hashOrder(Order calldata o) external pure returns (bytes32) {
        return OrderHash.hashOrder(o);
    }

    function acceptanceDigest(Order calldata o, bytes32 id, uint256 chainId, address swapic)
        external
        pure
        returns (bytes32)
    {
        return OrderHash.acceptanceDigest(o, id, chainId, swapic);
    }
}

/// The contracts against `canister/golden/interface.json`, the vectors the canister, the
/// services and the app are held to, computed with cast outside every codebase.
contract InterfaceGoldenTest is Test {
    string json;
    GoldenHarness h;

    function setUp() public {
        json = vm.readFile("../canister/golden/interface.json");
        h = new GoldenHarness();
    }

    function _check(string memory at) internal view returns (Order memory o) {
        bytes memory encoded = vm.parseJsonBytes(json, string.concat(at, ".abi_encode"));
        o = abi.decode(encoded, (Order));
        assertEq(abi.encode(o), encoded, "the order re-encodes to its golden bytes");
        uint256 chainId = vm.parseJsonUint(json, string.concat(at, ".chain_id"));
        address swapic = vm.parseJsonAddress(json, string.concat(at, ".swapic"));
        assertEq(h.hashOrder(o), vm.parseJsonBytes32(json, string.concat(at, ".struct_hash")));
        assertEq(
            OrderHash.domainSeparator(chainId, swapic),
            vm.parseJsonBytes32(json, string.concat(at, ".domain_separator"))
        );
        assertEq(h.orderId(o, chainId, swapic), vm.parseJsonBytes32(json, string.concat(at, ".order_id")));
    }

    function test_the_memo_order_is_the_golden_one() public view {
        Order memory o = _check(".order");
        assertEq(keccak256(o.memo), o.ref, "the memo hashes to the ref");
        assertEq(OrderHash.ORDER_TYPEHASH, vm.parseJsonBytes32(json, ".order.type_hash"));
        assertEq(
            keccak256(bytes(vm.parseJsonString(json, ".order.type_string"))),
            OrderHash.ORDER_TYPEHASH,
            "the type string, its referenced types appended"
        );
    }

    function test_the_empty_memo_order_is_the_golden_one() public view {
        Order memory o = _check(".order_empty_memo");
        assertEq(o.memo.length, 0);
    }

    function test_the_acceptance_digest_is_the_golden_one() public view {
        Order memory o = abi.decode(vm.parseJsonBytes(json, ".order.abi_encode"), (Order));
        bytes32 id = vm.parseJsonBytes32(json, ".order.order_id");
        assertEq(
            h.acceptanceDigest(o, id, vm.parseJsonUint(json, ".order.chain_id"), vm.parseJsonAddress(json, ".order.swapic")),
            vm.parseJsonBytes32(json, ".order.doors.acceptance_2612.digest")
        );
    }

    function test_the_event_and_the_doors_are_the_golden_ones() public view {
        assertEq(ISwapic.OrderStarted.selector, vm.parseJsonBytes32(json, ".order.order_started_topic0"));
        assertEq(
            bytes32(Swapic.run.selector), bytes32(bytes4(vm.parseJsonBytes(json, ".order.selectors.run")))
        );
        assertEq(
            bytes32(Swapic.runFor.selector), bytes32(bytes4(vm.parseJsonBytes(json, ".order.selectors.runFor")))
        );
        assertEq(
            bytes32(Swapic.runWithPermit2.selector),
            bytes32(bytes4(vm.parseJsonBytes(json, ".order.selectors.runWithPermit2")))
        );
    }
}
