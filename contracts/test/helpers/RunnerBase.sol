// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "../../src/Runner.sol";
import "../../src/SwapicTypes.sol";
import "../mocks/RailMocks.sol";
import "../mocks/TestToken.sol";

/// A Runner deployed from a stub Swapic, and the plan pieces the tests use.
abstract contract RunnerBase is Test {
    SwapicStub stub;
    Runner runner;
    TestToken usdc;
    TestToken weth;
    WethMock wrapped;
    DexMock dex;
    CctpMessengerMock messenger;
    NestedRailMock nested;
    CkHelperMock helper;
    address receiver;

    address user = address(0xA11CE);
    address canister = address(0xCA);
    address attacker = address(0xBAD);
    address victim = address(0x71C);

    function setUp() public virtual {
        vm.warp(1_790_000_000);
        stub = new SwapicStub();
        receiver = address(new SwapicStub());
        usdc = new TestToken();
        weth = new TestToken();
        wrapped = new WethMock();
        dex = new DexMock();
        messenger = new CctpMessengerMock();
        nested = new NestedRailMock();
        helper = new CkHelperMock();
        runner = _deployRunner(address(0));
        usdc.transfer(address(dex), 1e24);
        weth.transfer(address(dex), 1e24);
        vm.deal(address(dex), 1e24);
    }

    function _deployRunner(address ck) internal returns (Runner r) {
        r = new Runner(address(stub), receiver, ck, address(wrapped));
        stub.set(false, canister, address(r));
    }

    function _noWrites() internal pure returns (Write[] memory) {
        return new Write[](0);
    }

    function _write(uint32 at, uint256 minus) internal pure returns (Write[] memory ws) {
        ws = new Write[](1);
        ws[0] = Write(at, minus);
    }

    function _swapStep(address tokenIn, address tokenOut, uint256 amountIn, uint256 out)
        internal
        view
        returns (Step memory)
    {
        return Step(
            address(dex), abi.encodeCall(DexMock.swap, (tokenIn, tokenOut, amountIn, out)), tokenIn, amountIn, _noWrites()
        );
    }

    function _feeStep(uint256 fee) internal view returns (Step memory) {
        return Step(address(usdc), abi.encodeCall(IERC20.transfer, (canister, fee)), address(0), 0, _noWrites());
    }

    function _burnStep(uint256 min, uint256 minus) internal view returns (Step memory) {
        return Step(
            address(messenger),
            abi.encodeCall(
                CctpMessengerMock.depositForBurnWithHook,
                (0, 3, bytes32(uint256(1)), address(usdc), bytes32(uint256(1)), 1000, 1000, hex"01")
            ),
            address(usdc),
            min,
            _write(4, minus)
        );
    }

    function _order(address tokenIn, uint256 amountIn, uint256 steps) internal view returns (Order memory o) {
        o.ref = keccak256("ref");
        o.payer = user;
        o.tokenIn = tokenIn;
        o.amountIn = amountIn;
        o.deadline = block.timestamp + 60;
        o.steps = new Step[](steps);
    }

    /// What Swapic does before `execute`: the baseline, then the pull.
    /// A NATIVE tokenIn arrives wrapped.
    function _pull(Order memory o) internal returns (uint256 inBaseline) {
        if (o.tokenIn == NATIVE) {
            inBaseline = wrapped.balanceOf(address(runner));
            vm.deal(address(this), o.amountIn);
            wrapped.deposit{value: o.amountIn}();
            wrapped.transfer(address(runner), o.amountIn);
            return inBaseline;
        }
        inBaseline = IERC20(o.tokenIn).balanceOf(address(runner));
        IERC20(o.tokenIn).transfer(address(runner), o.amountIn);
    }

    function _run(Order memory o, uint256 inBaseline) internal {
        vm.prank(address(stub));
        runner.execute(o, keccak256("id"), inBaseline);
    }

    function _exec(Order memory o) internal {
        _run(o, _pull(o));
    }
}
