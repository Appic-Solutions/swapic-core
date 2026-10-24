// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Permit} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import "../../src/Swapic.sol";
import "../../src/Runner.sol";
import "../../src/SwapicTypes.sol";
import {Envelope} from "../../src/libraries/Envelope.sol";
import {CctpPlan} from "../helpers/CctpPlan.sol";
import {PORTAL, FEE} from "../helpers/TestConstants.sol";

/// Deploys Swapic from a fixed fresh deployer at nonce 0, so the Runner's
/// address is known in advance (it is the KyberSwap recipient in the
/// fixtures). Skips when the RPC variable is absent (plain `forge test`).
/// Run under the fork profile, `FOUNDRY_PROFILE=fork forge test --match-path
/// 'test/fork/*'`: KyberSwap's executor on Base uses Cancun opcodes, which the
/// default shanghai target leaves unactivated in the fork.
abstract contract ForkBase is Test {
    address constant DEPLOYER = 0x5a91c1000000000000000000000000000000d3b1;
    address constant SWAPIC = 0xF2E9d13Ba690c71cfF7788DE7273Ca7658d6D7C7;
    address constant RECEIVER = 0xf761327AD745400a3BD63DC7fA367DB58e43DE45;
    address constant RUNNER = 0xd9a6d4123BcF737A36e5f66D474891f15E8d2d3C;

    address constant MESSENGER = CctpPlan.TOKEN_MESSENGER;
    address constant TRANSMITTER = 0x81D40F21F12A8F0E3252Bccb954D722d4c464B64;
    address constant SCALER = 0x2f577A41BeC1BE1152AeEA12e73b7391d15f655D;
    bytes32 constant DEPOSIT_FOR_BURN =
        keccak256("DepositForBurn(address,uint256,address,bytes32,uint32,bytes32,bytes32,uint256,uint32,bytes)");
    /// USDC's ReceiveWithAuthorization type hash (EIP-3009)
    bytes32 constant RECEIVE_TYPEHASH = 0xd099cc98ef71107a616c4f0f941f04c322d8e254fe26b3c6668db87aae413de8;

    address constant USDC_BASE = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    address constant WETH_BASE = 0x4200000000000000000000000000000000000006;
    address constant USDC_ARB = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant WETH_ARB = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;

    uint256 constant PAYER_KEY = 0xA11CE;
    address payer = vm.addr(PAYER_KEY);
    address canister = address(0xCA11);

    Swapic swapic;
    Runner runner;

    /// false (and the test skipped) when the RPC variable is not set
    function _fork(string memory chain, string memory envVar, uint256 blockNumber) internal returns (bool) {
        if (bytes(vm.envOr(envVar, string(""))).length == 0) {
            vm.skip(true);
            return false;
        }
        vm.createSelectFork(chain, blockNumber);
        return true;
    }

    function _deploy(address usdc, address wrapped) internal {
        _deploy(usdc, wrapped, address(0));
    }

    function _deploy(address usdc, address wrapped, address ckHelper) internal {
        assertEq(vm.getNonce(DEPLOYER), 0, "the deployer is fresh on this chain");
        vm.prank(DEPLOYER);
        swapic = new Swapic(usdc, TRANSMITTER, MESSENGER, wrapped, ckHelper, address(this), address(0), canister);
        assertEq(address(swapic), SWAPIC, "the precomputed Swapic");
        assertEq(swapic.cctpReceiver(), RECEIVER, "the precomputed receiver");
        assertEq(swapic.runner(), RUNNER, "the precomputed Runner: the fixtures pay it");
        runner = Runner(payable(RUNNER));
    }

    function _fixture(string memory name) internal view returns (string memory) {
        return vm.readFile(string.concat("test/fixtures/", name, ".json"));
    }

    function _envelope(bytes32 ref, Action action, address recipient, address tokenOut, uint256 minOut)
        internal
        view
        returns (bytes memory)
    {
        return Envelope.encode(
            Envelope.Data(3, ref, action, uint64(block.timestamp + 1 days), recipient, tokenOut, minOut, 0, 0, 0)
        );
    }

    function _assertRunnerEmpty(address a, address b) internal view {
        assertEq(IERC20(a).balanceOf(RUNNER), 0, "runner holds nothing");
        assertEq(IERC20(b).balanceOf(RUNNER), 0, "runner holds nothing");
        assertEq(RUNNER.balance, 0);
        assertEq(IERC20(a).balanceOf(SWAPIC), 0);
        assertEq(IERC20(b).balanceOf(SWAPIC), 0);
    }

    /// Calls `target` with `cd` (encoded before the first `gasleft()`, so the
    /// delta is the call alone: no caller-side encoding or memory growth) and
    /// logs its gas. Bubbles a revert. Set any prank before calling this.
    function _call(string memory label, uint256 forkBlock, address target, bytes memory cd)
        internal
        returns (bytes memory ret)
    {
        bool ok;
        uint256 g = gasleft();
        assembly ("memory-safe") {
            ok := call(gas(), target, 0, add(cd, 32), mload(cd), 0, 0)
        }
        uint256 used = g - gasleft();
        assembly ("memory-safe") {
            ret := mload(0x40)
            mstore(ret, returndatasize())
            returndatacopy(add(ret, 32), 0, returndatasize())
            mstore(0x40, add(add(ret, 32), and(add(returndatasize(), 31), not(31))))
        }
        if (!ok) {
            assembly ("memory-safe") {
                revert(add(ret, 32), mload(ret))
            }
        }
        _gas(label, forkBlock, used, cd);
    }

    /// Execution gas (from the `gasleft()` delta around the call) and the
    /// calldata gas (4 per zero byte, 16 per other) the transaction adds on
    /// top of 21,000.
    function _gas(string memory label, uint256 forkBlock, uint256 used, bytes memory cd) internal pure {
        uint256 cg;
        for (uint256 i; i < cd.length; ++i) {
            cg += cd[i] == 0 ? 4 : 16;
        }
        console2.log(string.concat("GAS ", label, " block ", vm.toString(forkBlock)));
        console2.log(
            string.concat(
                "GAS   exec ",
                vm.toString(used),
                " calldata ",
                vm.toString(cd.length),
                " B ",
                vm.toString(cg),
                " total ",
                vm.toString(21_000 + cg + used)
            )
        );
    }

    /// The payer's EIP-3009 ReceiveWithAuthorization of the order on Base USDC.
    function _auth3009(Order memory o) internal view returns (GaslessAuth memory a) {
        a.door = Door.Auth3009;
        a.validBefore = block.timestamp + 1 hours;
        bytes32 structHash =
            keccak256(abi.encode(RECEIVE_TYPEHASH, o.payer, SWAPIC, o.amountIn, 0, a.validBefore, swapic.orderId(o)));
        a.signature = _sig(
            PAYER_KEY, keccak256(abi.encodePacked("\x19\x01", IERC20Permit(USDC_BASE).DOMAIN_SEPARATOR(), structHash))
        );
    }

    function _sig(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }
}
