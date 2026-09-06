// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/EvilRouterMock.sol";
import "./mocks/ReentrantRouterMock.sol";
import "./mocks/GasHogMock.sol";

contract VaultBatchTest is Test {
    Vault vault;
    TestToken a;
    TestToken b;
    SwapRouterMock router;
    EvilRouterMock evilRouter;
    ReentrantRouterMock reentrant;
    GasHogMock hog;
    address canister = address(0xCA);
    address guardian = address(0x6A);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        a = new TestToken();
        b = new TestToken();
        router = new SwapRouterMock();
        evilRouter = new EvilRouterMock();
        reentrant = new ReentrantRouterMock(vault);
        hog = new GasHogMock();

        a.transfer(address(vault), 1000e18);
        b.transfer(address(router), 1000e18);

        vm.startPrank(canister);
        vault.setRouterAllowlist(address(router), true);
        vault.setRouterAllowlist(address(evilRouter), true);
        vault.setRouterAllowlist(address(reentrant), true);
        vault.setRouterAllowlist(address(hog), true);
        vm.stopPrank();
    }

    function goodItem(bytes32 ref, uint256 amountIn, uint256 amountOut, uint256 gasLimit_)
        internal
        view
        returns (Vault.Item memory)
    {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), amountIn, amountOut)),
            address(a),
            amountIn
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](2);
        deltas[0] = Vault.Delta(address(a), -int256(amountIn));
        deltas[1] = Vault.Delta(address(b), int256(amountOut));
        return Vault.Item(ref, calls, deltas, gasLimit_);
    }

    function evilItem(bytes32 ref, uint256 amountIn, uint256 gasLimit_) internal view returns (Vault.Item memory) {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(evilRouter),
            0,
            abi.encodeCall(EvilRouterMock.swapAtoB, (address(a), address(b), amountIn, 0)),
            address(a),
            amountIn
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(address(b), int256(1)); // evil router never sends b: delta always missed
        return Vault.Item(ref, calls, deltas, gasLimit_);
    }

    function reentrantItem(bytes32 ref) internal view returns (Vault.Item memory) {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(address(reentrant), 0, abi.encodeCall(ReentrantRouterMock.attack, ()), address(0), 0);
        return Vault.Item(ref, calls, new Vault.Delta[](0), 0);
    }

    function hogItem(bytes32 ref, uint256 gasLimit_) internal view returns (Vault.Item memory) {
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(address(hog), 0, abi.encodeCall(GasHogMock.burn, ()), address(0), 0);
        return Vault.Item(ref, calls, new Vault.Delta[](0), gasLimit_);
    }

    function test_failed_item_does_not_revert_batch() public {
        Vault.Item[] memory items = new Vault.Item[](3);
        items[0] = goodItem("s1", 100e18, 95e18, 0);
        items[1] = evilItem("s2", 100e18, 0);
        items[2] = goodItem("s3", 100e18, 95e18, 0);

        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("s1", true);
        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("s2", false);
        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("s3", true);

        vm.prank(canister);
        vault.executeMany(items);

        assertEq(b.balanceOf(address(vault)), 190e18, "both good items' output landed");
    }

    function test_failed_item_state_rolls_back() public {
        uint256 beforeA = a.balanceOf(address(vault));

        Vault.Item[] memory items = new Vault.Item[](1);
        items[0] = evilItem("e1", 100e18, 0);

        // proves the pull actually happened (and was then rolled back), not
        // that the item failed before ever touching state
        vm.expectCall(address(evilRouter), abi.encodeCall(EvilRouterMock.swapAtoB, (address(a), address(b), 100e18, 0)));
        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("e1", false);

        vm.prank(canister);
        vault.executeMany(items); // must not revert the batch

        assertEq(a.balanceOf(address(vault)), beforeA, "pulled amount rolled back");
        assertEq(a.allowance(address(vault), address(evilRouter)), 0, "approval rolled back");
    }

    function test_reentrant_target_blocked() public {
        Vault.Item[] memory items = new Vault.Item[](2);
        items[0] = reentrantItem("r1");
        items[1] = goodItem("r2", 100e18, 95e18, 0);

        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("r1", false);
        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("r2", true);

        vm.prank(canister);
        vault.executeMany(items);

        assertEq(b.balanceOf(address(vault)), 95e18, "later item unaffected by reentry attempt");
        // depositNative has no canister gate and no other revert condition
        // active here (not paused, fresh quote hash): only the nonReentrant
        // guard held by the outer executeMany can explain this not landing.
        assertFalse(vault.usedQuoteHash("reentrant-deposit"), "reentrant deposit blocked by reentrancy guard");
    }

    function test_run_item_self_only() public {
        Vault.Item memory someItem = goodItem("x", 1e18, 1e18, 0);
        vm.expectRevert(Vault.OnlySelf.selector);
        vault.runItem(someItem);
    }

    function test_gas_capped_item_fails_alone() public {
        Vault.Item[] memory items = new Vault.Item[](2);
        items[0] = hogItem("g1", 200_000);
        items[1] = goodItem("g2", 100e18, 95e18, 0);

        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("g1", false);
        vm.expectEmit(true, false, false, true, address(vault));
        emit Vault.ItemResult("g2", true);

        vm.prank(canister);
        vault.executeMany(items);

        assertEq(b.balanceOf(address(vault)), 95e18, "item after capped gas-hog still runs");
    }

    function test_multicall_is_atomic_and_keeps_sender() public {
        Vault.Call[] memory goodCalls = new Vault.Call[](1);
        goodCalls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(a), address(b), 100e18, 95e18)),
            address(a),
            100e18
        );
        Vault.Delta[] memory goodDeltas = new Vault.Delta[](2);
        goodDeltas[0] = Vault.Delta(address(a), -int256(100e18));
        goodDeltas[1] = Vault.Delta(address(b), int256(95e18));

        Vault.Call[] memory badCalls = new Vault.Call[](1);
        badCalls[0] = Vault.Call(
            address(evilRouter),
            0,
            abi.encodeCall(EvilRouterMock.swapAtoB, (address(a), address(b), 100e18, 0)),
            address(a),
            100e18
        );
        Vault.Delta[] memory badDeltas = new Vault.Delta[](1);
        badDeltas[0] = Vault.Delta(address(b), int256(1));

        bytes[] memory selfCalls = new bytes[](2);
        selfCalls[0] = abi.encodeCall(Vault.execute, ("m1", goodCalls, goodDeltas));
        selfCalls[1] = abi.encodeCall(Vault.execute, ("m2", badCalls, badDeltas));

        uint256 beforeA = a.balanceOf(address(vault));
        uint256 beforeB = b.balanceOf(address(vault));

        // if multicall did not preserve msg.sender via delegatecall, this would
        // revert with OnlyCanister on the first payload instead of DeltaMissed
        // on the second: reaching DeltaMissed proves the sender was kept.
        vm.prank(canister);
        vm.expectRevert(Vault.DeltaMissed.selector);
        vault.multicall(selfCalls);

        assertEq(a.balanceOf(address(vault)), beforeA, "first payload's pull rolled back");
        assertEq(b.balanceOf(address(vault)), beforeB, "first payload's output rolled back");
    }
}
