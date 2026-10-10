// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicRouter.sol";
import "../src/SwapicTypes.sol";
import "./mocks/RailMocks.sol";
import "./mocks/AuthorizationToken.sol";

/// The gasless doors on a Circle-shaped stable (EIP-3009 and EIP-2612).
contract SwapicRouterGaslessTest is Test {
    SwapicRouter router;
    AuthorizationToken usdc;
    CctpMessengerMock messenger;

    uint256 userKey = 0xA11CE;
    address user = vm.addr(userKey);
    uint256 strangerKey = 0xBADBEEF;
    address relayer = address(0xCA);
    uint64 constant ARB = 42161;

    function setUp() public {
        vm.warp(1_790_000_000);
        usdc = new AuthorizationToken();
        messenger = new CctpMessengerMock();
        router = new SwapicRouter(address(usdc), address(messenger), address(0), address(this), address(0x6A), relayer);
        router.setCctpDestination(ARB, 3, bytes32(uint256(uint160(address(0xE0E0)))), true);
        usdc.transfer(user, 1000e6);
    }

    function _params() internal view returns (SwapParams memory p) {
        p.ref = keccak256("gasless");
        p.payer = user;
        p.tokenIn = address(usdc);
        p.amountIn = 100e6;
        p.deadline = block.timestamp + 45;
        p.legMinOut = 100e6;
        p.maxGasFee = 50_000;
        p.rail = Rail.Cctp;
        p.dstChainId = ARB;
        p.maxFee = 10_000;
        p.minFinality = 1000;
        p.dst.action = Action.Deliver;
        p.dst.recipient = bytes32(uint256(uint160(user)));
        p.dst.minOut = 99e6;
    }

    function _sig(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _auth3009(uint256 key, bytes32 nonce, uint256 value) internal view returns (GaslessAuth memory a) {
        uint256 validBefore = block.timestamp + 1 hours;
        bytes32 structHash = keccak256(
            abi.encode(usdc.RECEIVE_WITH_AUTHORIZATION_TYPEHASH(), user, address(router), value, 0, validBefore, nonce)
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", usdc.DOMAIN_SEPARATOR(), structHash));
        a.door = Door.Auth3009;
        a.gasFee = 20_000;
        a.validBefore = validBefore;
        a.signature = _sig(key, digest);
    }

    function test_3009_door_binds_the_recomputed_quote_hash() public {
        SwapParams memory p = _params();
        GaslessAuth memory a = _auth3009(userKey, router.quoteHash(p), p.amountIn);
        vm.prank(relayer);
        router.swapFor(p, a);
        assertEq(usdc.balanceOf(relayer), 20_000, "gas reimbursed in the stable");
        (uint256 burned,,,,,,,) = messenger.last();
        assertEq(burned, 100e6 - 20_000);
        assertEq(usdc.balanceOf(address(router)), 0);
    }

    function test_an_authorization_cannot_be_paired_with_other_params() public {
        SwapParams memory p = _params();
        GaslessAuth memory a = _auth3009(userKey, router.quoteHash(p), p.amountIn);
        p.dst.recipient = bytes32(uint256(uint160(address(0xBAD))));
        vm.prank(relayer);
        vm.expectRevert("FiatTokenV2: invalid signature");
        router.swapFor(p, a);
    }

    function test_only_the_relayer_may_call_and_the_gas_fee_is_capped() public {
        SwapParams memory p = _params();
        GaslessAuth memory a = _auth3009(userKey, router.quoteHash(p), p.amountIn);
        vm.expectRevert(ISwapicRouter.NotRelayer.selector);
        router.swapFor(p, a);
        a.gasFee = p.maxGasFee + 1;
        vm.prank(relayer);
        vm.expectRevert(ISwapicRouter.GasFeeTooHigh.selector);
        router.swapFor(p, a);
    }

    function test_2612_door_needs_the_payers_acceptance() public {
        SwapParams memory p = _params();
        bytes32 permitDigest = keccak256(
            abi.encodePacked(
                "\x19\x01",
                usdc.DOMAIN_SEPARATOR(),
                keccak256(
                    abi.encode(
                        keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                        user,
                        address(router),
                        p.amountIn,
                        usdc.nonces(user),
                        p.deadline
                    )
                )
            )
        );
        GaslessAuth memory a;
        a.door = Door.Permit2612;
        a.signature = _sig(userKey, permitDigest);
        a.acceptance = _sig(strangerKey, router.acceptanceDigest(p));
        vm.prank(relayer);
        vm.expectRevert(ISwapicRouter.QuoteNotAccepted.selector);
        router.swapFor(p, a);

        a.acceptance = _sig(userKey, router.acceptanceDigest(p));
        vm.prank(relayer);
        router.swapFor(p, a);
        assertEq(usdc.balanceOf(user), 900e6);
        assertEq(usdc.balanceOf(address(router)), 0);
    }
}
