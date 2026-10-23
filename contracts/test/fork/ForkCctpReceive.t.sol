// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "./ForkBase.sol";
import "../../src/CctpReceiver.sol";
import "../helpers/CctpPlan.sol";

interface IAttestable {
    function attesterManager() external view returns (address);
    function enableAttester(address) external;
    function setSignatureThreshold(uint256) external;
    function signatureThreshold() external view returns (uint256);
}

interface IMessageTransmitter {
    function receiveMessage(bytes calldata message, bytes calldata attestation) external returns (bool);
}

/// The destination side against the real MessageTransmitterV2, TokenMessengerV2
/// and TokenMinterV2: the attester manager enables a test attester and sets
/// the threshold to 1; the messages are real CCTP v2 burn messages from Base
/// (domain 6, whose TokenMessenger is the registered remote), signed by it.
abstract contract RealTransmitter is ForkBase {
    uint256 constant ATTESTER_KEY = 0xA77E57;
    uint32 constant BASE_DOMAIN = 6;
    uint256 nonce;
    CctpReceiver receiver;

    function _attester() internal {
        receiver = CctpReceiver(RECEIVER);
        IAttestable t = IAttestable(TRANSMITTER);
        vm.startPrank(t.attesterManager());
        t.enableAttester(vm.addr(ATTESTER_KEY));
        if (t.signatureThreshold() != 1) t.setSignatureThreshold(1);
        vm.stopPrank();
    }

    function _message(uint32 dstDomain, address mintRecipient, address caller, uint256 amount, uint256 fee, bytes memory env)
        internal
        returns (bytes memory)
    {
        return CctpPlan.message(
            BASE_DOMAIN, dstDomain, keccak256(abi.encode("p9c", ++nonce)), caller, mintRecipient, USDC_BASE, amount, fee, env
        );
    }

    function _attest(uint256 key, bytes memory m) internal pure returns (bytes memory) {
        return _sig(key, keccak256(m));
    }
}

contract ForkCctpReceiveTest is RealTransmitter {
    string j;
    uint256 forkBlock;

    function setUp() public {
        j = _fixture("kyber_arb_usdc_weth_100");
        forkBlock = vm.parseJsonUint(j, ".block");
        if (!_fork("arbitrum", "ARBITRUM_RPC_URL", forkBlock)) return;
        _deploy(USDC_ARB, WETH_ARB);
        _attester();
    }

    function _swapEnvelope(uint256 minOut) internal view returns (bytes memory) {
        return _envelope(keccak256("real cctp"), Action.Swap, payer, WETH_ARB, minOut);
    }

    function test_complete_with_a_real_kyber_recipe_then_replay_fails() public {
        uint256 quoted = vm.parseJsonUint(j, ".amountOut");
        // 100 USDC minted after a 0.01 USDC fast fee: exactly the recipe's amount
        bytes memory m = _message(3, RECEIVER, RECEIVER, 100_010_000, 10_000, _swapEnvelope(quoted * 97 / 100));
        bytes memory a = _attest(ATTESTER_KEY, m);
        Recipe memory r = Recipe(vm.parseJsonAddress(j, ".routerAddress"), vm.parseJsonBytes(j, ".data"), address(0));

        bytes memory cd = abi.encodeCall(CctpReceiver.completeCctp, (m, a, r));
        vm.prank(canister);
        _call("completeCctp swap (real transmitter, KyberSwap 100 USDC)", forkBlock, RECEIVER, cd);

        assertGe(IERC20(WETH_ARB).balanceOf(payer), quoted * 97 / 100, "the user got the WETH");
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 0, "and no USDC");
        assertEq(IERC20(USDC_ARB).balanceOf(RECEIVER), 0, "receiver empty");
        _assertRunnerEmpty(USDC_ARB, WETH_ARB);

        vm.prank(canister);
        vm.expectRevert("Nonce already used");
        receiver.completeCctp(m, a, r);
        vm.warp(block.timestamp + 2 days);
        vm.expectRevert("Nonce already used");
        receiver.release(m, a);
    }

    function test_an_unknown_attester_is_refused() public {
        bytes memory m = _message(3, RECEIVER, RECEIVER, 100e6, 0, _swapEnvelope(1));
        Recipe memory r = Recipe(address(0), "", address(0));
        vm.prank(canister);
        vm.expectRevert("Invalid signature: not attester");
        receiver.completeCctp(m, _attest(0xBAD, m), r);
    }

    function test_release_after_the_deadline() public {
        bytes memory m = _message(3, RECEIVER, RECEIVER, 50_005_000, 5_000, _swapEnvelope(1));
        bytes memory a = _attest(ATTESTER_KEY, m);
        vm.expectRevert(ICctpReceiver.DeadlineNotPassed.selector);
        receiver.release(m, a);

        vm.warp(block.timestamp + 1 days + 1);
        bytes memory cd = abi.encodeCall(CctpReceiver.release, (m, a));
        vm.prank(address(0x5742A9E));
        _call("release (real transmitter)", forkBlock, RECEIVER, cd);
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 50e6, "all of the mint");
        assertEq(IERC20(USDC_ARB).balanceOf(RECEIVER), 0);

        vm.expectRevert("Nonce already used");
        receiver.release(m, a);
    }

    function test_plain_deliver_receive_message() public {
        bytes memory env = _envelope(keccak256("deliver"), Action.Deliver, payer, address(0), 0);
        bytes memory m = _message(3, payer, address(0), 100_010_000, 10_000, env);
        bytes memory a = _attest(ATTESTER_KEY, m);
        bytes memory cd = abi.encodeCall(IMessageTransmitter.receiveMessage, (m, a));
        vm.prank(canister);
        _call("receiveMessage Deliver (real transmitter)", forkBlock, TRANSMITTER, cd);
        assertEq(IERC20(USDC_ARB).balanceOf(payer), 100e6);
    }
}

/// Ethereum: a CkDeposit arrival through the real transmitter into the real
/// ck minter's deposit helper.
contract ForkCkDepositTest is RealTransmitter {
    uint256 constant ETH_BLOCK = 26_198_252;
    address constant CK_HELPER = 0x18901044688D3756C35Ed2b36D93e6a5B8e00E68;
    address constant USDC_ETH = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
    address constant WETH_ETH = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
    bytes32 constant RECEIVED = keccak256("ReceivedEthOrErc20(address,address,uint256,bytes32,bytes32)");

    function setUp() public {
        if (!_fork("mainnet", "ETH_RPC_URL", ETH_BLOCK)) return;
        _deploy(USDC_ETH, WETH_ETH, CK_HELPER);
        _attester();
    }

    function test_complete_ck_deposit_into_the_real_helper() public {
        bytes32 principal = bytes32(uint256(0x1d0d9e40)) << 200;
        bytes32 subaccount = bytes32(uint256(7));
        bytes memory env = Envelope.encode(
            Envelope.Data(3, keccak256("ck"), Action.CkDeposit, uint64(block.timestamp + 1 days), payer, address(0), 0, principal, subaccount, 0)
        );
        bytes memory m = _message(0, RECEIVER, RECEIVER, 25_000_000, 0, env);
        bytes memory a = _attest(ATTESTER_KEY, m);
        Recipe memory r = Recipe(address(0), "", address(0));
        address minter = ICkMinterHelper(CK_HELPER).getMinterAddress();
        uint256 before = IERC20(USDC_ETH).balanceOf(minter);

        bytes memory cd = abi.encodeCall(CctpReceiver.completeCctp, (m, a, r));
        vm.recordLogs();
        vm.prank(canister);
        _call("completeCctp CkDeposit (real transmitter, real ck helper)", ETH_BLOCK, RECEIVER, cd);

        assertEq(IERC20(USDC_ETH).balanceOf(minter) - before, 25e6, "all of it to the ck minter");
        assertEq(IERC20(USDC_ETH).balanceOf(payer), 0, "no refund");
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bool seen;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == CK_HELPER && logs[i].topics[0] == RECEIVED) {
                assertEq(logs[i].topics[3], principal);
                seen = true;
            }
        }
        assertTrue(seen, "the helper logged the deposit for the principal");
        assertEq(IERC20(USDC_ETH).balanceOf(RECEIVER), 0);
        assertEq(IERC20(USDC_ETH).balanceOf(RUNNER), 0);
    }
}

interface ICkMinterHelper {
    function getMinterAddress() external view returns (address);
}
