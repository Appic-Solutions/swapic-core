// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";

/// no receive/fallback: every native send to it fails
contract RejectEth {}

contract VaultPayoutTest is Test {
    Vault vault;
    TestToken token;
    RejectEth rejecter;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    address alice = makeAddr("alice");

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        token = new TestToken();
        token.transfer(address(vault), 1000e18);
        vm.deal(address(vault), 10 ether);

        rejecter = new RejectEth();
    }

    function test_erc20_payout_moves_tokens_and_emits() public {
        vm.expectEmit(true, true, true, true);
        emit Vault.Payout("s1", address(token), alice, 100e18);
        vm.prank(canister);
        vault.payout("s1", address(token), alice, 100e18);

        assertEq(token.balanceOf(alice), 100e18, "recipient credited");
        assertEq(token.balanceOf(address(vault)), 900e18, "vault debited");
    }

    function test_native_payout_moves_eth() public {
        vm.prank(canister);
        vault.payout("s2", address(0), alice, 1 ether);

        assertEq(alice.balance, 1 ether, "recipient credited");
        assertEq(address(vault).balance, 9 ether, "vault debited");
    }

    function test_refund_emits_its_own_event() public {
        vm.expectEmit(true, true, true, true);
        emit Vault.Refunded("r1", address(token), alice, 50e18);
        vm.prank(canister);
        vault.refund("r1", address(token), alice, 50e18);

        assertEq(token.balanceOf(alice), 50e18, "refund landed");
    }

    function test_only_canister_can_payout_or_refund() public {
        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.payout("s3", address(token), alice, 1e18);

        vm.expectRevert(Vault.OnlyCanister.selector);
        vault.refund("r3", address(token), alice, 1e18);
    }

    function test_paused_payouts_blocks_both_doors() public {
        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Payouts);

        vm.prank(canister);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.payout("s4", address(token), alice, 1e18);

        vm.prank(canister);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.refund("r4", address(token), alice, 1e18);
    }

    function test_native_send_to_rejecting_contract_reverts() public {
        vm.prank(canister);
        vm.expectRevert(Vault.SendFailed.selector);
        vault.payout("s5", address(0), address(rejecter), 1 ether);
    }
}
