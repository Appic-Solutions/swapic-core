// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import "../helpers/CctpPlan.sol";

interface IPermit2Domain {
    function DOMAIN_SEPARATOR() external view returns (bytes32);
}

/// Base: `runFor` on real USDC (EIP-3009, EIP-2612) and the real Permit2, with
/// a `vm.sign` key: the fee step pays the canister, the CCTP step burns the
/// rest; any change to the plan breaks the authorization.
contract ForkGaslessTest is ForkBase {
    string constant WITNESS_TYPE =
        "OrderWitness witness)OrderWitness(bytes32 orderId)TokenPermissions(address token,uint256 amount)";
    uint256 forkBlock;

    function setUp() public {
        string memory j = _fixture("kyber_base_weth_usdc");
        forkBlock = vm.parseJsonUint(j, ".block");
        if (!_fork("base", "BASE_RPC_URL", forkBlock)) return;
        _deploy(USDC_BASE, WETH_BASE);
        deal(USDC_BASE, payer, 300e6);
    }

    function _order(bytes32 ref) internal view returns (Order memory o) {
        o.ref = ref;
        o.payer = payer;
        o.tokenIn = USDC_BASE;
        o.amountIn = 100e6;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](2);
        o.steps[0] =
            Step(USDC_BASE, abi.encodeCall(IERC20.transfer, (canister, FEE)), address(0), 0, new Write[](0));
        o.steps[1] = CctpPlan.burnStep(
            MESSENGER, USDC_BASE, 99e6, 3, RECEIVER, RECEIVER, 100_000, 1000, _envelope(ref, Action.Swap, payer, WETH_ARB, 1)
        );
    }

    function _auth2612(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Permit2612;
        bytes32 permit = keccak256(
            abi.encode(
                keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)"),
                payer,
                SWAPIC,
                o.amountIn,
                IERC20Permit(USDC_BASE).nonces(payer),
                o.deadline
            )
        );
        a.signature =
            _sig(PAYER_KEY, keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC_BASE).DOMAIN_SEPARATOR(), permit)));
        a.acceptance = _sig(PAYER_KEY, swapic.acceptanceDigest(o));
    }

    function _authPermit2(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Permit2;
        a.nonce = 1;
        a.deadline = block.timestamp + 1 hours;
        bytes32 typeHash = keccak256(
            abi.encodePacked(
                "PermitWitnessTransferFrom(TokenPermissions permitted,address spender,uint256 nonce,uint256 deadline,",
                WITNESS_TYPE
            )
        );
        bytes32 witness = keccak256(abi.encode(keccak256("OrderWitness(bytes32 orderId)"), swapic.orderId(o)));
        bytes32 permitted =
            keccak256(abi.encode(keccak256("TokenPermissions(address token,uint256 amount)"), o.tokenIn, o.amountIn));
        bytes32 structHash = keccak256(abi.encode(typeHash, permitted, SWAPIC, a.nonce, a.deadline, witness));
        a.signature = _sig(
            PAYER_KEY, keccak256(abi.encodePacked("\x19\x01", IPermit2Domain(PERMIT2).DOMAIN_SEPARATOR(), structHash))
        );
    }

    function _runAndCheck(Order memory o, GaslessAuth memory a, string memory label) internal {
        uint256 before = IERC20(USDC_BASE).balanceOf(payer);
        bytes memory cd = abi.encodeCall(Swapic.runFor, (o, a));
        vm.recordLogs();
        vm.prank(canister);
        _call(label, forkBlock, SWAPIC, cd);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 burned;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == MESSENGER && logs[i].topics[0] == DEPOSIT_FOR_BURN) {
                (burned) = abi.decode(logs[i].data, (uint256));
            }
        }
        assertEq(burned, 100e6 - FEE, "the rest is burned");
        assertEq(before - IERC20(USDC_BASE).balanceOf(payer), 100e6, "exactly amountIn");
        _assertRunnerEmpty(USDC_BASE, WETH_BASE);
    }

    function test_3009_on_real_usdc() public {
        Order memory o = _order(keccak256("3009"));
        GaslessAuth memory a = _auth3009(o);
        _runAndCheck(o, a, "runFor 3009: canister fee + CCTP burn");
        assertEq(IERC20(USDC_BASE).balanceOf(canister), FEE, "the fee step paid the canister");
    }

    function test_2612_on_real_usdc() public {
        Order memory o = _order(keccak256("2612"));
        GaslessAuth memory a = _auth2612(o);
        _runAndCheck(o, a, "runFor 2612: canister fee + CCTP burn");
        assertEq(IERC20(USDC_BASE).balanceOf(canister), FEE);
    }

    function test_permit2_on_real_permit2() public {
        vm.prank(payer);
        IERC20(USDC_BASE).approve(PERMIT2, type(uint256).max);
        Order memory o = _order(keccak256("permit2"));
        GaslessAuth memory a = _authPermit2(o);
        _runAndCheck(o, a, "runFor Permit2: canister fee + CCTP burn");
        assertEq(IERC20(USDC_BASE).balanceOf(canister), FEE);
    }

    function test_changed_plan_rejected_on_real_tokens() public {
        vm.prank(payer);
        IERC20(USDC_BASE).approve(PERMIT2, type(uint256).max);
        Order memory o = _order(keccak256("changed"));
        GaslessAuth memory a3009 = _auth3009(o);
        GaslessAuth memory a2612 = _auth2612(o);
        GaslessAuth memory aP2 = _authPermit2(o);
        Order memory b = _order(keccak256("changed"));
        b.steps[0].data = abi.encodeCall(IERC20.transfer, (canister, FEE * 2));

        vm.startPrank(canister);
        vm.expectRevert("FiatTokenV2: invalid signature");
        swapic.runFor(b, a3009);
        vm.expectRevert(ISwapic.NotAccepted.selector);
        swapic.runFor(b, a2612);
        vm.expectRevert(bytes4(keccak256("InvalidSigner()")));
        swapic.runFor(b, aP2);
        swapic.runFor(o, a3009);
        vm.expectRevert(ISwapic.Used.selector);
        swapic.runFor(o, aP2);
        vm.stopPrank();
    }

    /// Base WETH (WETH9: a fallback that deposits, so any selector succeeds)
    /// with garbage signature bytes: the 3009 door must not treat the no-op as
    /// a receipt, nor pay from Swapic's stray balance.
    function test_3009_on_weth_with_garbage_signature_reverts() public {
        deal(WETH_BASE, SWAPIC, 1e18);
        Order memory o = _order(keccak256("weth"));
        o.tokenIn = WETH_BASE;
        o.amountIn = 1e18;
        GaslessAuth memory a;
        a.door = Door.Auth3009;
        a.validBefore = block.timestamp + 1 hours;
        a.signature = abi.encodePacked(bytes32(uint256(0x1111)), bytes32(uint256(0x2222)), uint8(27));
        (bool ok,) = WETH_BASE.call(
            abi.encodeWithSelector(0xef55bec6, payer, SWAPIC, uint256(1e18), uint256(0), a.validBefore, bytes32(0), uint8(27), bytes32(0), bytes32(0))
        );
        assertTrue(ok, "WETH accepts receiveWithAuthorization as a no-op");
        vm.prank(canister);
        vm.expectRevert(ISwapic.NotReceived.selector);
        swapic.runFor(o, a);
        assertEq(IERC20(WETH_BASE).balanceOf(SWAPIC), 1e18, "the stray WETH stays put");
    }
}

