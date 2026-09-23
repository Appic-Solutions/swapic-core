// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/EcoPortalMock.sol";

/// The one allowlist. The vault has no public execution door, so the only
/// calls it ever issues are the canister's, through `execute` and
/// `executeMany` (and `multicall` over them), with calldata the canister chose.
/// That is what lets a target hold balances the vault can claim: Eco's Portal
/// escrows a reward whose creator is the vault, and only the vault may name
/// where `refundTo` sends it. The Portal is the worked example here: the
/// canister reaches it, and no stranger can make the vault call it.
///
/// The list lives in main's slot with main's setter, so an upgrade from main
/// carries every entry across unchanged.
contract VaultAllowlistTest is Test {
    Vault vault;
    TestToken usdc;
    TestToken b;
    EcoPortalMock portal;
    SwapRouterMock router;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    address attacker = address(0xBAD);

    bytes32 constant ROUTE = keccak256("route");
    uint256 constant POOLED = 1000e18;
    /// the slot of main's `allowedRouter` mapping, from `forge inspect Vault
    /// storage-layout` on main at b909658
    uint256 constant ALLOWLIST_SLOT = 4;
    /// the removed public door's exact signature, called the only way it still can be
    bytes4 constant DEPOSIT_AND_EXECUTE = bytes4(
        keccak256(
            "depositAndExecute(bytes32,address,uint256,(address,uint256,bytes,address,uint256)[],address,uint256,address)"
        )
    );

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        usdc = new TestToken();
        b = new TestToken();
        portal = new EcoPortalMock();
        router = new SwapRouterMock();

        usdc.transfer(address(vault), POOLED); // what earlier deposits left behind
        b.transfer(address(router), 1e24);

        vm.startPrank(canister);
        vault.setRouterAllowlist(address(portal), true);
        vault.setRouterAllowlist(address(router), true);
        vm.stopPrank();
    }

    function _reward(uint256 amount) internal view returns (EcoPortalMock.Reward memory) {
        return EcoPortalMock.Reward(uint64(block.timestamp + 1 hours), address(vault), address(usdc), amount);
    }

    function _publish(EcoPortalMock.Reward memory reward) internal view returns (Vault.Call[] memory calls) {
        calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(portal),
            0,
            abi.encodeCall(EcoPortalMock.publishAndFund, (ROUTE, reward)),
            address(usdc),
            reward.amount
        );
    }

    function _swap(uint256 amountIn, uint256 amountOut) internal view returns (Vault.Call[] memory calls) {
        calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(usdc), address(b), amountIn, amountOut)),
            address(usdc),
            amountIn
        );
    }

    function _usdcSpend(uint256 amount) internal view returns (Vault.Delta[] memory deltas) {
        deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(address(usdc), -int256(amount));
    }

    /// Every door that makes the vault issue calldata its caller chose, tried
    /// by `from` with the same calls, each expected to refuse for its own
    /// reason. The entry doors are not in this list: they issue only fixed
    /// token calls to the token the depositor names. The removed public door
    /// is tried with `publicDoor`, whatever that caller would have sent it, and
    /// must answer with a bare revert: no function is left there.
    function _everyDoorRefuses(address from, Vault.Call[] memory calls, bytes memory publicDoor) internal {
        Vault.Delta[] memory none = new Vault.Delta[](0);
        Vault.Item[] memory items = new Vault.Item[](1);
        items[0] = Vault.Item("stranger", calls, none, 0);
        bytes[] memory selfCalls = new bytes[](1);
        selfCalls[0] = abi.encodeCall(Vault.execute, ("stranger", calls, none));
        bytes memory onlyCanister = abi.encodeWithSelector(Vault.OnlyCanister.selector);

        vm.startPrank(from);
        _refused(abi.encodeCall(Vault.execute, ("stranger", calls, none)), onlyCanister, "execute");
        _refused(abi.encodeCall(Vault.executeMany, (items)), onlyCanister, "executeMany");
        _refused(abi.encodeCall(Vault.multicall, (selfCalls)), onlyCanister, "multicall");
        _refused(abi.encodeCall(Vault.runItem, (items[0])), abi.encodeWithSelector(Vault.OnlySelf.selector), "runItem");
        _refused(publicDoor, "", "the removed public door");
        vm.stopPrank();
    }

    function _refused(bytes memory data, bytes memory reason, string memory door) internal {
        (bool ok, bytes memory ret) = address(vault).call(data);
        assertFalse(ok, string.concat("a stranger's call landed through ", door));
        assertEq(ret, reason, string.concat("refused for another reason at ", door));
    }

    // ----------------------------------------------- the worked example: Eco's Portal

    /// The canister's side: it publishes a reward out of the pool with the vault
    /// as creator, and the vault is the one the reward goes back to.
    function test_the_eco_portal_is_reachable_through_execute() public {
        EcoPortalMock.Reward memory reward = _reward(100e18);

        vm.prank(canister);
        vault.execute("swap-1", _publish(reward), _usdcSpend(100e18));

        assertEq(usdc.balanceOf(address(portal)), 100e18, "the reward is escrowed");
        assertTrue(portal.escrowed(portal.intentHash(ROUTE, reward)), "the intent is funded");
        assertEq(usdc.allowance(address(vault), address(portal)), 0, "no approval left standing");

        vm.warp(reward.deadline + 1);
        portal.refund(ROUTE, reward); // permissionless, and it pays the creator
        assertEq(usdc.balanceOf(address(vault)), POOLED, "the refund came home to the vault");
    }

    function test_the_eco_portal_is_reachable_through_execute_many() public {
        Vault.Item[] memory items = new Vault.Item[](1);
        items[0] = Vault.Item("swap-1", _publish(_reward(100e18)), _usdcSpend(100e18), 0);

        vm.expectEmit(address(vault));
        emit Vault.ItemResult("swap-1", true);

        vm.prank(canister);
        vault.executeMany(items);

        assertEq(usdc.balanceOf(address(portal)), 100e18, "the batch item published");
    }

    /// The exploit the Eco memo found: an expired, unfilled reward whose creator
    /// is the vault, and a call to `refundTo(.., attacker)`, which only the
    /// creator may make. A stranger cannot make the vault make it, through any
    /// door, and the canister then reclaims the reward home.
    function test_a_stranger_cannot_make_the_vault_refund_to_them() public {
        EcoPortalMock.Reward memory reward = _reward(100e18);
        vm.prank(canister);
        vault.execute("swap-1", _publish(reward), _usdcSpend(100e18));
        vm.warp(reward.deadline + 1);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.refundTo, (ROUTE, reward, attacker)), address(0), 0
        );
        // what the removed door would have been handed: a one-unit deposit that
        // funds the call, the shape its own rules asked for
        bytes memory publicDoor = abi.encodeWithSelector(
            DEPOSIT_AND_EXECUTE, bytes32("q1"), address(usdc), uint256(1), calls, address(usdc), uint256(0), attacker
        );
        _everyDoorRefuses(attacker, calls, publicDoor);

        assertEq(usdc.balanceOf(attacker), 0, "the attacker took nothing");
        assertEq(usdc.balanceOf(address(portal)), 100e18, "the reward is still escrowed");

        // and the canister, the creator's only voice, reclaims it home
        calls[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.refundTo, (ROUTE, reward, address(vault))), address(0), 0
        );
        vm.prank(canister);
        vault.execute("reclaim-1", calls, new Vault.Delta[](0));
        assertEq(usdc.balanceOf(address(vault)), POOLED, "the reward came home");
    }

    /// The property the whole list rests on: whoever the caller is, if it is not
    /// the canister, it cannot make the vault call a listed target. The removed
    /// door is offered the honest swap it used to accept, a deposit of the
    /// caller's own spent in full through a listed router, so that door's
    /// absence is the only thing that can refuse it.
    function testFuzz_a_stranger_cannot_make_the_vault_call_a_target(address stranger) public {
        vm.assume(stranger != canister && stranger != address(vault));

        usdc.transfer(stranger, 10e18);
        vm.prank(stranger);
        usdc.approve(address(vault), type(uint256).max);

        uint256 vaultUsdc = usdc.balanceOf(address(vault));
        uint256 routerB = b.balanceOf(address(router));

        Vault.Call[] memory drain = _swap(POOLED, 0); // takes the pool, pays nothing back
        Vault.Call[] memory honest = _swap(10e18, 9e18);
        bytes memory publicDoor = abi.encodeWithSelector(
            DEPOSIT_AND_EXECUTE, bytes32("q1"), address(usdc), uint256(10e18), honest, address(b), uint256(0), stranger
        );
        _everyDoorRefuses(stranger, drain, publicDoor);

        assertEq(usdc.balanceOf(address(vault)), vaultUsdc, "the pool moved");
        assertEq(b.balanceOf(address(router)), routerB, "the router was reached");
        assertEq(usdc.allowance(address(vault), address(router)), 0, "an approval was left standing");
    }

    // ------------------------------------------------------------------ the list as such

    /// On, the canister reaches the target; off, it does not.
    function test_taking_a_target_off_the_allowlist() public {
        Vault.Call[] memory calls = _swap(10e18, 9e18);

        vm.prank(canister);
        vault.execute("swap-1", calls, _usdcSpend(10e18));
        assertEq(b.balanceOf(address(vault)), 9e18, "the canister reaches a listed router");

        vm.prank(canister);
        vault.setRouterAllowlist(address(router), false);
        assertFalse(vault.allowedRouter(address(router)), "off the list");

        vm.prank(canister);
        vm.expectRevert(Vault.TargetNotAllowed.selector);
        vault.execute("swap-2", calls, new Vault.Delta[](0));
    }

    /// The log records what was stored, and the getter reads it back.
    function test_the_setter_logs_what_it_stored() public {
        address target = address(0xE1);
        vm.startPrank(canister);

        vm.expectEmit(address(vault));
        emit Vault.AllowlistSet(target, true);
        vault.setRouterAllowlist(target, true);
        assertTrue(vault.allowedRouter(target), "on");

        vm.expectEmit(address(vault));
        emit Vault.AllowlistSet(target, false);
        vault.setRouterAllowlist(target, false);
        assertFalse(vault.allowedRouter(target), "off");

        vm.stopPrank();
    }

    // ------------------------------------------------------------------ main's layout

    /// An entry written exactly as main's vault stores it is on the list, with
    /// its whole reach. So an upgrade from main carries every entry across.
    function test_an_entry_written_in_mains_slot_is_allowed() public {
        SwapRouterMock legacy = new SwapRouterMock();
        b.transfer(address(legacy), 100e18);
        vm.store(address(vault), keccak256(abi.encode(address(legacy), ALLOWLIST_SLOT)), bytes32(uint256(1)));

        assertTrue(vault.allowedRouter(address(legacy)), "main's entry reads as allowed");

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(legacy),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(usdc), address(b), 10e18, 9e18)),
            address(usdc),
            10e18
        );
        vm.prank(canister);
        vault.execute("swap-1", calls, _usdcSpend(10e18));
        assertEq(b.balanceOf(address(vault)), 9e18, "main's entry swapped through execute");
    }

    /// The other half of the same pin: the setter writes main's slot and no
    /// other, so no slot of the tiers' layout is ever written again.
    function test_the_allowlist_lives_in_mains_slot_and_nowhere_else() public {
        address target = address(0xE1);
        bytes32 slot = keccak256(abi.encode(target, ALLOWLIST_SLOT));

        vm.record();
        vm.prank(canister);
        vault.setRouterAllowlist(target, true);
        (, bytes32[] memory writes) = vm.accesses(address(vault));
        assertEq(writes.length, 1, "one write");
        assertEq(writes[0], slot, "and it is main's slot");
        assertEq(vm.load(address(vault), slot), bytes32(uint256(1)), "on");

        vm.prank(canister);
        vault.setRouterAllowlist(target, false);
        assertEq(vm.load(address(vault), slot), bytes32(0), "off");
    }
}
