// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/SwapRouterMock.sol";
import "./mocks/EcoPortalMock.sol";
import "./mocks/HookToken.sol";

/// The security review's PoC A (vault-review-security C1), kept as a regression.
///
/// The vault pools every user's money, so it cannot tell the proceeds of one
/// caller's swap from any other payment that lands while that caller's code
/// runs. The public door paid out "what the vault gained" across a window in
/// which the caller's own token ran code, and that code called Eco's
/// permissionless `refund`, which pays the reward's creator: the vault. The
/// door then carried one user's escrowed reward out to the attacker, with
/// every rule it had satisfied honestly.
///
/// No rule on such a door can be made safe, so the door is gone. Its entry call
/// now reverts with no function behind the selector, the reward stays in
/// escrow, and it is still the vault's to reclaim through the canister.
contract VaultInflowCaptureTest is Test {
    Vault vault;
    TestToken usdc;
    EcoPortalMock portal;
    SwapRouterMock router;

    address canister = address(0xCA);
    address guardian = address(0x6A);
    address attacker = address(0xBAD);

    bytes32 constant ROUTE = keccak256("route");
    uint256 constant POOLED = 1000e18; // other users' money
    uint256 constant REWARD = 400e18; // one user's swap, locked as an Eco reward

    /// the removed public door's exact signature, called the only way it still can be
    bytes4 constant DEPOSIT_AND_EXECUTE = bytes4(
        keccak256(
            "depositAndExecute(bytes32,address,uint256,(address,uint256,bytes,address,uint256)[],address,uint256,address)"
        )
    );

    EcoPortalMock.Reward reward;

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        usdc = new TestToken();
        portal = new EcoPortalMock();
        router = new SwapRouterMock();

        usdc.transfer(address(vault), POOLED);

        vm.startPrank(canister);
        vault.setRouterAllowlist(address(portal), true);
        vault.setRouterAllowlist(address(router), true);
        vm.stopPrank();

        // the canister publishes one user's swap as an Eco intent, vault as creator
        reward = EcoPortalMock.Reward(uint64(block.timestamp + 1 hours), address(vault), address(usdc), REWARD);
        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.publishAndFund, (ROUTE, reward)), address(usdc), REWARD
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(address(usdc), -int256(REWARD));
        vm.prank(canister);
        vault.execute("eco-swap", calls, deltas);
        assertEq(usdc.balanceOf(address(vault)), POOLED - REWARD);

        // nobody filled it; the canister's reclaim is due, and so is everyone's
        vm.warp(reward.deadline + 1);
    }

    /// A: the deposit token is the attacker's own ordinary ERC20, one unit,
    /// approved in full and taken in full by an honest listed router. Its
    /// transfer out of the vault calls the Portal's permissionless refund.
    function test_poc_A_an_attacker_token_cannot_capture_an_eco_refund() public {
        HookToken hook = new HookToken();
        hook.transfer(attacker, 10);
        hook.arm(address(vault), address(portal), abi.encodeCall(EcoPortalMock.refund, (ROUTE, reward)));

        Vault.Call[] memory calls = new Vault.Call[](1);
        calls[0] = Vault.Call(
            address(router),
            0,
            abi.encodeCall(SwapRouterMock.swapAtoB, (address(hook), address(usdc), 1, 0)),
            address(hook),
            1
        );
        bytes memory attack = abi.encodeWithSelector(
            DEPOSIT_AND_EXECUTE, bytes32("x"), address(hook), uint256(1), calls, address(usdc), uint256(0), attacker
        );

        vm.startPrank(attacker);
        hook.approve(address(vault), 1);
        (bool ok, bytes memory ret) = address(vault).call(attack);
        vm.stopPrank();

        assertFalse(ok, "the public execution door still answers");
        assertEq(ret.length, 0, "a bare revert: no function behind the selector");
        assertEq(usdc.balanceOf(attacker), 0, "the attacker took nothing");
        assertEq(hook.balanceOf(attacker), 10, "the attacker's token never left them");
        assertEq(usdc.balanceOf(address(portal)), REWARD, "the reward is still escrowed");
        assertTrue(portal.escrowed(portal.intentHash(ROUTE, reward)), "the intent is still funded");

        // and it is still the vault's: the canister, the creator's only voice,
        // reclaims it home through the one door that runs calls
        Vault.Call[] memory reclaim = new Vault.Call[](1);
        reclaim[0] = Vault.Call(
            address(portal), 0, abi.encodeCall(EcoPortalMock.refundTo, (ROUTE, reward, address(vault))), address(0), 0
        );
        Vault.Delta[] memory deltas = new Vault.Delta[](1);
        deltas[0] = Vault.Delta(address(usdc), int256(REWARD));
        vm.prank(canister);
        vault.execute("eco-reclaim", reclaim, deltas);
        assertEq(usdc.balanceOf(address(vault)), POOLED, "the reward came home");
    }
}
