// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "../src/Vault.sol";
import "../src/interfaces/ISignatureTransfer.sol";

contract VaultPermit2Test is Test {
    ISignatureTransfer constant PERMIT2 = ISignatureTransfer(0x000000000022D473030F116dDEE9F6B43aC78BA3);
    address constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    uint256 constant FORK_BLOCK = 50960000;
    uint256 constant AMOUNT = 100e6;

    bytes32 constant WITNESS_TYPEHASH = keccak256(
        "PermitWitnessTransferFrom(TokenPermissions permitted,address spender,uint256 nonce,uint256 deadline,QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)"
    );

    string forkUrl;
    Vault vault;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);

    function setUp() public {
        forkUrl = vm.envOr("BASE_RPC_URL", string(""));
        if (bytes(forkUrl).length == 0) return; // CI has no RPC secret

        vm.createSelectFork(forkUrl, FORK_BLOCK);
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        deal(USDC, user, AMOUNT);
        vm.prank(user);
        IERC20(USDC).approve(address(PERMIT2), type(uint256).max);
    }

    function _permit(uint256 nonce, uint256 deadline)
        internal
        pure
        returns (ISignatureTransfer.PermitTransferFrom memory)
    {
        return ISignatureTransfer.PermitTransferFrom({
            permitted: ISignatureTransfer.TokenPermissions({token: USDC, amount: AMOUNT}),
            nonce: nonce,
            deadline: deadline
        });
    }

    function _sign(bytes32 quoteHash, uint256 nonce, uint256 deadline) internal view returns (bytes memory) {
        bytes32 permitted =
            keccak256(abi.encode(keccak256("TokenPermissions(address token,uint256 amount)"), USDC, AMOUNT));
        bytes32 witness = keccak256(abi.encode(keccak256("QuoteWitness(bytes32 quoteHash)"), quoteHash));
        bytes32 structHash =
            keccak256(abi.encode(WITNESS_TYPEHASH, permitted, address(vault), nonce, deadline, witness));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", PERMIT2.DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(userKey, digest);
        return abi.encodePacked(r, s, v);
    }

    function test_pull_with_permit2_witness_moves_usdc() public {
        if (bytes(forkUrl).length == 0) vm.skip(true);

        // the vault's witness type string must complete Permit2's typehash stub
        assertEq(
            keccak256(
                abi.encodePacked(
                    "PermitWitnessTransferFrom(TokenPermissions permitted,address spender,uint256 nonce,uint256 deadline,",
                    "QuoteWitness witness)QuoteWitness(bytes32 quoteHash)TokenPermissions(address token,uint256 amount)"
                )
            ),
            WITNESS_TYPEHASH
        );

        uint256 deadline = block.timestamp + 1 hours;
        bytes memory sig = _sign("q1", 0, deadline);

        vm.expectEmit(true, true, true, true);
        emit Vault.Deposited("q1", USDC, user, AMOUNT);
        vm.prank(canister);
        vault.pullWithPermit2("q1", user, _permit(0, deadline), sig);

        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT);
        assertEq(IERC20(USDC).balanceOf(user), 0);
        assertTrue(vault.quoteKeyUsed("q1", user));
    }

    function test_pull_with_permit2_rejects_other_quote_hash() public {
        if (bytes(forkUrl).length == 0) vm.skip(true);

        uint256 deadline = block.timestamp + 1 hours;
        bytes memory sig = _sign("q1", 0, deadline); // signature covers q1 only

        vm.expectRevert(bytes4(keccak256("InvalidSigner()")));
        vm.prank(canister);
        vault.pullWithPermit2("q2", user, _permit(0, deadline), sig);

        assertEq(IERC20(USDC).balanceOf(address(vault)), 0);
    }
}
