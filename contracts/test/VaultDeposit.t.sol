// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "./mocks/TestToken.sol";
import "./mocks/FeeOnTransferToken.sol";
import "./mocks/FallbackToken.sol";

contract VaultDepositTest is Test {
    Vault vault;
    TestToken token;
    FeeOnTransferToken feeToken;
    address canister = address(0xCA);
    address guardian = address(0x6A);

    function setUp() public {
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        token = new TestToken();
        feeToken = new FeeOnTransferToken();

        token.transfer(address(this), 1000e18);
        feeToken.transfer(address(this), 1000e18);
    }

    function test_deposit_pulls_tokens_and_emits() public {
        token.approve(address(vault), 100e18);
        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", address(token), address(this), 100e18);
        vault.deposit("q1", address(token), 100e18);
        assertEq(token.balanceOf(address(vault)), 100e18);
    }

    function test_duplicate_quote_hash_reverts() public {
        token.approve(address(vault), 200e18);
        vault.deposit("q1", address(token), 100e18);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.deposit("q1", address(token), 100e18);
    }

    /// Rewritten for the zero-delta rule: burning a key now costs the attacker
    /// a real deposit, one wei, since a deposit of nothing is refused.
    function test_stranger_cannot_burn_someone_elses_quote_hash() public {
        address attacker = address(0xBAD);
        vm.deal(attacker, 1);

        // attacker burns the hash under their own key, for one wei
        vm.prank(attacker);
        vault.depositNative{value: 1}("q9");
        assertTrue(vault.quoteKeyUsed("q9", attacker), "attacker's own key is marked");

        // the honest payer's quote is untouched and still usable
        assertFalse(vault.quoteKeyUsed("q9", address(this)), "honest payer's key is free");
        token.approve(address(vault), 200e18);
        vault.deposit("q9", address(token), 100e18);
        assertEq(token.balanceOf(address(vault)), 100e18, "honest deposit landed");

        // ...but that payer still cannot double-pay the same quote
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.deposit("q9", address(token), 100e18);
    }

    function test_fee_on_transfer_records_actual_amount() public {
        feeToken.approve(address(vault), 100e18);
        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q2", address(feeToken), address(this), 99e18);
        vault.deposit("q2", address(feeToken), 100e18);
        assertEq(feeToken.balanceOf(address(vault)), 99e18);
    }

    /// A deposit that receives nothing is refused, so it cannot burn the
    /// payer's quote key and log a Deposited of zero: an amount of zero on an
    /// honest token, and a token whose fallback "succeeds" at moving nothing.
    function test_a_deposit_that_receives_nothing_is_refused() public {
        token.approve(address(vault), 100e18);
        vm.expectRevert(Vault.NothingReceived.selector);
        vault.deposit("q5", address(token), 0);

        FallbackToken hollow = new FallbackToken();
        vm.expectRevert(Vault.NothingReceived.selector);
        vault.deposit("q5", address(hollow), 100e18);

        // the positive control: the same key takes a deposit that arrives
        vault.deposit("q5", address(token), 1);
        assertEq(token.balanceOf(address(vault)), 1, "a real deposit on the same key landed");
    }

    function test_a_native_deposit_of_nothing_is_refused() public {
        vm.expectRevert(Vault.NothingReceived.selector);
        vault.depositNative{value: 0}("q6");

        vault.depositNative{value: 1}("q6");
        assertTrue(vault.quoteKeyUsed("q6", address(this)), "one wei is a deposit");
    }

    function test_deposit_native() public {
        vault.depositNative{value: 1 ether}("q3");
        assertEq(address(vault).balance, 1 ether);
    }

    function test_deposit_blocked_when_paused() public {
        vm.prank(guardian);
        vault.pause(Vault.PauseClass.Deposits);
        token.approve(address(vault), 1e18);
        vm.expectRevert(Vault.IsPaused.selector);
        vault.deposit("q4", address(token), 1e18);
    }
}
