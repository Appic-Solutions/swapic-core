// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/Vault.sol";
import "./handlers/VaultHandler.sol";

contract VaultInvariantsTest is StdInvariant, Test {
    VaultHandler handler;
    Vault vault;
    address canister; // the handler itself: it plays the canister role
    address guardian = address(0x6A);

    function setUp() public {
        handler = new VaultHandler(guardian);
        vault = handler.vault();
        canister = address(handler);

        bytes4[] memory selectors = new bytes4[](7);
        selectors[0] = VaultHandler.user_deposit.selector;
        selectors[1] = VaultHandler.canister_execute.selector;
        selectors[2] = VaultHandler.canister_execute_many.selector;
        selectors[3] = VaultHandler.canister_payout.selector;
        selectors[4] = VaultHandler.user_atomic_swap.selector;
        selectors[5] = VaultHandler.user_atomic_retention.selector;
        selectors[6] = VaultHandler.user_atomic_over_approve.selector;

        targetContract(address(handler));
        targetSelector(StdInvariant.FuzzSelector({addr: address(handler), selectors: selectors}));
    }

    /// no call sequence may leave the vault standing as a spender on a router
    function invariant_no_router_allowance_survives() public view {
        address[2] memory routers = [address(handler.router()), address(handler.evilRouter())];
        for (uint256 i = 0; i < 3; i++) {
            TestToken t = handler.tokens(i);
            for (uint256 j = 0; j < routers.length; j++) {
                assertEq(t.allowance(address(vault), routers[j]), 0, "router allowance left standing");
            }
        }
    }

    /// the handler credits a token only for flows the vault is supposed to make:
    /// deposits and swap proceeds in, payouts/refunds/atomic payouts and swap
    /// inputs out. Anything else moving the balance is leakage.
    function invariant_balances_match_sanctioned_flows() public view {
        for (uint256 i = 0; i < 3; i++) {
            address t = address(handler.tokens(i));
            int256 expected = int256(handler.ghostIn(t)) - int256(handler.ghostOut(t));
            int256 actual = int256(handler.tokens(i).balanceOf(address(vault)));
            assertGe(actual, expected, "tokens left the vault outside the sanctioned exits");
            assertEq(actual, expected, "vault balance drifted from the sanctioned flows");
        }
    }

    /// the vault never holds native here, so a stray native balance means some
    /// call route moved value that the ERC20 ghosts cannot see
    function invariant_vault_holds_no_native() public view {
        assertEq(address(vault).balance, 0, "unexplained native balance");
    }

    /// without this the invariants above could pass vacuously on a handler whose
    /// entry points all revert into their catch arms
    function test_handler_entry_points_all_reach_the_vault() public {
        handler.user_deposit(0, 100e18, 1);
        handler.canister_execute(0, 1, 50e18, 40e18, 1e18);
        handler.canister_execute_many(3, 7, 0);
        handler.canister_payout(0, 10e18, 2, false);
        handler.canister_payout(0, 10e18, 2, true);
        handler.user_atomic_swap(0, 1, 100e18, 60e18, 55e18, 3, false);
        handler.user_atomic_retention(1, 20e18, 4);
        handler.user_atomic_over_approve(0, 1, 1e18, 500e18, 5);

        assertEq(handler.deposits(), 1, "deposit landed");
        assertEq(handler.executes(), 1, "execute landed");
        assertGt(handler.batchItems(), 0, "at least one batch item landed");
        assertEq(handler.payouts(), 2, "payout and refund landed");
        assertEq(handler.atomicSwaps(), 1, "atomic swap landed");
        assertEq(handler.retentions(), 1, "atomic retention landed");
        assertEq(handler.overApprovesRejected(), 1, "over-approve rejected");
        assertEq(handler.overApprovesAccepted(), 0, "over-approve never accepted");

        invariant_no_router_allowance_survives();
        invariant_balances_match_sanctioned_flows();
    }

    /// every entry point that marks a quote must reject a pair already spent,
    /// whatever the rest of its arguments look like: _markQuote runs first, so
    /// zero-filled payloads are enough to reach it
    function testFuzz_spent_quote_key_is_rejected_everywhere(bytes32 quoteHash, address payer, address other) public {
        vm.assume(payer != other);
        address token = address(handler.tokens(0));

        vm.prank(payer);
        vault.depositNative(quoteHash);
        assertTrue(vault.quoteKeyUsed(quoteHash, payer), "pair burned");

        vm.prank(payer);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.deposit(quoteHash, token, 0);

        vm.prank(payer);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.depositNative(quoteHash);

        vm.prank(payer);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.depositAndExecute(quoteHash, token, 0, new Vault.Call[](0), token, 0, address(0));

        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithPermit(quoteHash, token, payer, 0, 0, 0, bytes32(0), bytes32(0));

        ISignatureTransfer.PermitTransferFrom memory permit = ISignatureTransfer.PermitTransferFrom({
            permitted: ISignatureTransfer.TokenPermissions({token: token, amount: 0}),
            nonce: 0,
            deadline: 0
        });
        vm.prank(canister);
        vm.expectRevert(Vault.QuoteHashUsed.selector);
        vault.pullWithPermit2(quoteHash, payer, permit, "");

        // the mark is per payer, so the same hash is still open for anyone else
        vm.prank(other);
        vault.depositNative(quoteHash);
        assertTrue(vault.quoteKeyUsed(quoteHash, other), "other payer unaffected");
    }
}
