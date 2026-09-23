// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/EcoPortalMock.sol";

/// The two tiers. The public tier (the strict one) holds stateless swap routers
/// and is all the public `depositAndExecute` door may call. The canister tier
/// adds targets only the canister's own doors may call, such as an escrow whose
/// exits the vault itself is authorized for. Eco's Portal is the worked example:
/// the canister must be able to publish and reclaim through it, and no stranger
/// may ever make the vault call it.
///
/// The public door's own spend rule (VaultPublicDrain.t.sol) is the other,
/// independent defence. The refusals here are built to pass that rule, so the
/// tier is the only thing that can be refusing them, and each one has a control
/// showing the identical call landing once the target is on the public tier.
contract VaultAllowlistTiersTest is Test {
    Vault vault;
    TestToken usdc;
    TestToken b;
    EcoPortalMock portal;
    SwapRouterMock router;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    address attacker = address(0xBAD);
    address user = address(0xB0B);

    bytes32 constant ROUTE = keccak256("route");
    uint256 constant POOLED = 1000e18;
    /// the slot of the single allowlist mapping before the tiers, from
    /// `forge inspect Vault storage-layout` at the commit before them
    uint256 constant ALLOWLIST_SLOT = 4;

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        usdc = new TestToken();
        b = new TestToken();
        portal = new EcoPortalMock();
        router = new SwapRouterMock();

        usdc.transfer(address(vault), POOLED); // what earlier deposits left behind
        usdc.transfer(attacker, 10e18);
        usdc.transfer(user, 100e18);
        b.transfer(address(router), 1e24);

        vm.startPrank(canister);
        vault.setRouterAllowlist(address(portal), true, false); // the canister tier
        vault.setRouterAllowlist(address(router), true, true); // the public tier
        vm.stopPrank();

        vm.prank(attacker);
        usdc.approve(address(vault), type(uint256).max);
        vm.prank(user);
        usdc.approve(address(vault), type(uint256).max);
    }

    function _reward(uint256 amount) internal view returns (EcoPortalMock.Reward memory) {
        return EcoPortalMock.Reward(uint64(block.timestamp + 1 hours), address(vault), address(usdc), amount);
    }

    /// Approves exactly the reward and the portal takes exactly the reward, so
    /// the call satisfies the public door's spend rule in full.
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

    function _swap(address via, uint256 amountIn, uint256 amountOut)
        internal
        view
        returns (Vault.Call[] memory calls)
    {
        calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            via,
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

    /// The public side, with a call the door's spend rule accepts in full: a
    /// real deposit, approved in full, to a target that takes every unit of it.
    /// Only the tier can refuse it.
    function test_the_eco_portal_is_refused_through_the_public_door() public {
        Vault.Call[] memory calls = _publish(_reward(10e18));

        vm.prank(attacker);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 0, address(0));

        assertEq(usdc.balanceOf(address(portal)), 0, "nothing reached the portal");
        assertEq(usdc.balanceOf(attacker), 10e18, "the deposit rolled back");
    }

    /// The control for the refusal above, and the reason the Portal must never
    /// sit on the public tier: the identical call lands, and a stranger has made
    /// the vault the creator of an intent the canister never published.
    function test_the_same_call_lands_once_the_portal_is_on_the_public_tier() public {
        EcoPortalMock.Reward memory reward = _reward(10e18);
        Vault.Call[] memory calls = _publish(reward);

        vm.prank(canister);
        vault.setRouterAllowlist(address(portal), true, true); // never do this

        vm.prank(attacker);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 0, address(0));

        assertEq(usdc.balanceOf(address(portal)), 10e18, "a stranger published in the vault's name");
        assertTrue(portal.escrowed(portal.intentHash(ROUTE, reward)), "an intent the canister never made");
    }

    /// The exploit the Eco memo found, tried against the tier: an expired,
    /// unfilled reward whose creator is the vault, and a public call to
    /// `refundTo(.., attacker)`. The spend rule would also stop this (refundTo
    /// takes nothing), but the tier stops it first, before any call runs.
    function test_refund_to_a_stranger_is_refused_through_the_public_door() public {
        EcoPortalMock.Reward memory reward = _reward(100e18);
        vm.prank(canister);
        vault.execute("swap-1", _publish(reward), _usdcSpend(100e18));
        vm.warp(reward.deadline + 1);

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.refundTo, (ROUTE, reward, attacker)), address(usdc), 1e18
        );
        vm.prank(attacker);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q1", address(usdc), 1e18, calls, address(b), 0, address(0));

        assertEq(usdc.balanceOf(attacker), 10e18, "the attacker took nothing");
        assertEq(usdc.balanceOf(address(portal)), 100e18, "the reward is still escrowed");

        // and the canister, the creator's only other voice, reclaims it home
        calls[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.refundTo, (ROUTE, reward, address(vault))), address(0), 0
        );
        vm.prank(canister);
        vault.execute("reclaim-1", calls, new Vault.Delta[](0));
        assertEq(usdc.balanceOf(address(vault)), POOLED, "the reward came home");
    }

    // ------------------------------------------------------------- the tiers as such

    /// A target on neither tier is refused through the public door with the same
    /// error it always had.
    function test_an_unlisted_target_is_still_not_allowed_through_the_public_door() public {
        SwapRouterMock stray = new SwapRouterMock();
        b.transfer(address(stray), 100e18);
        Vault.Call[] memory calls = _swap(address(stray), 10e18, 9e18);

        vm.prank(user);
        vm.expectRevert(Vault.TargetNotAllowed.selector);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 0, user);
    }

    function test_a_target_is_never_public_without_being_allowed() public {
        vm.prank(canister);
        vault.setRouterAllowlist(address(router), false, true);

        assertFalse(vault.allowedRouter(address(router)), "not allowed");
        assertFalse(vault.allowedPublicRouter(address(router)), "so not public either");

        Vault.Call[] memory calls = _swap(address(router), 10e18, 9e18);
        vm.prank(user);
        vm.expectRevert(Vault.TargetNotAllowed.selector);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 0, user);
    }

    /// One target walked through every tier: public (both doors), canister
    /// (execute only), and off (neither).
    function test_moving_a_target_between_tiers() public {
        Vault.Call[] memory calls = _swap(address(router), 10e18, 9e18);

        vm.prank(user);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 9e18, user);
        assertEq(b.balanceOf(user), 9e18, "the public door reaches a public-tier router");

        vm.prank(canister);
        vault.setRouterAllowlist(address(router), true, false);
        assertTrue(vault.allowedRouter(address(router)), "still on the canister tier");
        assertFalse(vault.allowedPublicRouter(address(router)), "off the public tier");

        vm.prank(user);
        vm.expectRevert(Vault.PublicCallNotAllowed.selector);
        vault.depositAndExecute("q2", address(usdc), 10e18, calls, address(b), 9e18, user);

        vm.prank(canister);
        vault.execute("swap-1", calls, _usdcSpend(10e18));
        assertEq(b.balanceOf(address(vault)), 9e18, "the canister still reaches it");

        vm.prank(canister);
        vault.setRouterAllowlist(address(router), false, false);
        vm.prank(canister);
        vm.expectRevert(Vault.TargetNotAllowed.selector);
        vault.execute("swap-2", calls, new Vault.Delta[](0));
    }

    /// The log records the tier that was stored, not the flags that were sent:
    /// a public flag without the allow flag stores nothing, and says so.
    function test_the_setter_logs_the_tier_it_stored() public {
        address target = address(0xE1);
        vm.startPrank(canister);

        vm.expectEmit(address(vault));
        emit Vault.AllowlistSet(target, true, false);
        vault.setRouterAllowlist(target, true, false);

        vm.expectEmit(address(vault));
        emit Vault.AllowlistSet(target, true, true);
        vault.setRouterAllowlist(target, true, true);

        vm.expectEmit(address(vault));
        emit Vault.AllowlistSet(target, false, false);
        vault.setRouterAllowlist(target, false, true);

        vm.stopPrank();
    }

    // ------------------------------------------------------------------ the migration

    /// An entry written by the vault before the tiers existed, exactly as the
    /// single allowlist stored it, is on the public tier and keeps its whole
    /// reach, the public door included. The migration is the layout itself, so
    /// an upgrade of a live vault would carry every entry across unchanged.
    function test_an_entry_made_before_the_tiers_is_on_the_public_tier() public {
        SwapRouterMock legacy = new SwapRouterMock();
        b.transfer(address(legacy), 100e18);
        vm.store(address(vault), keccak256(abi.encode(address(legacy), ALLOWLIST_SLOT)), bytes32(uint256(1)));

        assertTrue(vault.allowedRouter(address(legacy)), "the canister still reaches it");
        assertTrue(vault.allowedPublicRouter(address(legacy)), "and so does the public door");

        Vault.Call[] memory calls = _swap(address(legacy), 10e18, 9e18);
        vm.prank(user);
        vault.depositAndExecute("q1", address(usdc), 10e18, calls, address(b), 9e18, user);
        assertEq(b.balanceOf(user), 9e18, "the pre-tier entry swapped through the public door");
    }

    /// The other half of the same pin: the public tier is written to that slot,
    /// and a canister-tier entry is not.
    function test_the_public_tier_is_the_slot_the_single_allowlist_used() public {
        address target = address(0xE1);
        bytes32 slot = keccak256(abi.encode(target, ALLOWLIST_SLOT));

        vm.prank(canister);
        vault.setRouterAllowlist(target, true, true);
        assertEq(vm.load(address(vault), slot), bytes32(uint256(1)), "the public tier lives in the old slot");

        vm.prank(canister);
        vault.setRouterAllowlist(target, true, false);
        assertEq(vm.load(address(vault), slot), bytes32(0), "a canister-tier entry is not in it");
        assertTrue(vault.allowedRouter(target), "but it is allowed");
    }
}
