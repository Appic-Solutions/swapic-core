// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import "../src/Vault.sol";
import "../src/interfaces/IERC3009.sol";

/// Both gasless doors against the real Circle USDC on a Base fork. This is the
/// only place the digest work is proved end to end: the mocks agree with the
/// standard, and the standard is not what a user's wallet signs against, the
/// deployed token is. Skips itself without BASE_RPC_URL, like VaultPermit2.
contract VaultForkUSDCTest is Test {
    address constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    uint256 constant FORK_BLOCK = 50960000;
    uint256 constant AMOUNT = 100e6;

    /// keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
    bytes32 constant RECEIVE_TYPEHASH = 0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;
    string forkUrl;
    Vault vault;
    address canister = address(0xCA);
    address guardian = address(0x6A);
    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address stranger = vm.addr(strangerKey);

    function setUp() public {
        forkUrl = vm.envOr("BASE_RPC_URL", string(""));
        if (bytes(forkUrl).length == 0) return; // CI has no RPC secret

        vm.createSelectFork(forkUrl, FORK_BLOCK);
        Vault impl = new Vault();
        bytes memory init = abi.encodeCall(Vault.initialize, (canister, guardian));
        vault = Vault(payable(address(new ERC1967Proxy(address(impl), init))));

        deal(USDC, user, AMOUNT);
    }

    function _skipWithoutFork() internal {
        if (bytes(forkUrl).length == 0) vm.skip(true);
    }

    function _signAuthorization(uint256 key, uint256 validAfter, uint256 validBefore, bytes32 nonce)
        internal
        view
        returns (Vault.Signature memory)
    {
        bytes32 structHash =
            keccak256(abi.encode(RECEIVE_TYPEHASH, user, address(vault), AMOUNT, validAfter, validBefore, nonce));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC).DOMAIN_SEPARATOR(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return Vault.Signature(v, r, s);
    }

    // ---------------------------------------------------------------- EIP-3009

    function test_fork_3009_pull_with_the_quote_hash_as_the_nonce() public {
        _skipWithoutFork();

        uint256 validBefore = block.timestamp + 1 hours;
        vm.prank(canister);
        vault.pullWithAuthorization(
            "q1", USDC, user, AMOUNT, 0, validBefore, _signAuthorization(userKey, 0, validBefore, "q1")
        );

        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the pull landed");
        assertEq(IERC20(USDC).balanceOf(user), 0, "the user paid");
        assertTrue(IERC3009(USDC).authorizationState(user, "q1"), "the token burned the quote hash");
        // the door's whole claim: no standing authorization is left behind
        assertEq(IERC20(USDC).allowance(user, address(vault)), 0, "an allowance was created");
    }

    function test_fork_3009_cannot_be_front_run_by_anyone() public {
        _skipWithoutFork();

        uint256 validBefore = block.timestamp + 1 hours;
        Vault.Signature memory sig = _signAuthorization(userKey, 0, validBefore, "q1");

        vm.prank(stranger);
        vm.expectRevert("FiatTokenV2: caller must be the payee");
        IERC3009(USDC).receiveWithAuthorization(user, address(vault), AMOUNT, 0, validBefore, "q1", sig.v, sig.r, sig.s);

        vm.prank(canister);
        vault.pullWithAuthorization("q1", USDC, user, AMOUNT, 0, validBefore, sig);
        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "the payee's own pull still lands");
    }

    function test_fork_3009_forged_signature_is_refused() public {
        _skipWithoutFork();

        vm.prank(user);
        IERC20(USDC).approve(address(vault), type(uint256).max); // the worst case

        uint256 validBefore = block.timestamp + 1 hours;
        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        vault.pullWithAuthorization(
            "q1", USDC, user, AMOUNT, 0, validBefore, _signAuthorization(strangerKey, 0, validBefore, "q1")
        );

        assertEq(IERC20(USDC).balanceOf(address(vault)), 0, "a standing allowance moved nothing");
    }

    /// Circle's bounds are strict on both sides, so the canister's window has to
    /// leave a second at each end. Equality reverts on chain, not in the canister.
    function test_fork_3009_window_bounds_are_strict() public {
        _skipWithoutFork();

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is not yet valid");
        vault.pullWithAuthorization(
            "q1",
            USDC,
            user,
            AMOUNT,
            block.timestamp,
            block.timestamp + 1 hours,
            _signAuthorization(userKey, block.timestamp, block.timestamp + 1 hours, "q1")
        );

        vm.prank(canister);
        vm.expectRevert("FiatTokenV2: authorization is expired");
        vault.pullWithAuthorization(
            "q2", USDC, user, AMOUNT, 0, block.timestamp, _signAuthorization(userKey, 0, block.timestamp, "q2")
        );

        vm.prank(canister);
        vault.pullWithAuthorization(
            "q3",
            USDC,
            user,
            AMOUNT,
            block.timestamp - 1,
            block.timestamp + 1,
            _signAuthorization(userKey, block.timestamp - 1, block.timestamp + 1, "q3")
        );
        assertEq(IERC20(USDC).balanceOf(address(vault)), AMOUNT, "one second either side works");
    }
}
