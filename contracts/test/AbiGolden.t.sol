// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicTypes.sol";
import {QuoteHash} from "../src/libraries/QuoteHash.sol";
import {HookData} from "../src/libraries/HookData.sol";

/// The ABI's golden vectors, the ones the services and the canister reproduce.
/// The quote hash is checked against foundry's own EIP-712 encoder fed the
/// JSON typed data a client sends (`vm.eip712HashTypedData`), so a wrong type
/// string or nested-type order cannot pass. Every field is non-zero.
contract AbiGoldenTest is Test {
    uint256 constant CHAIN = 8453;
    address constant ROUTER = 0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB;

    /// pinned after the cross-check against vm.eip712HashTypedData; any change
    /// to the encoding moves it, and abi.md publishes it
    bytes32 constant GOLDEN_QUOTE_HASH = 0x157e2444cb085aade4fca1374efc685c1ab6447c39f19d12e41c5af8ce30518e;

    function _params() internal pure returns (SwapParams memory p) {
        p.ref = bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111));
        p.payer = 0x2222222222222222222222222222222222222222;
        p.tokenIn = 0x3333333333333333333333333333333333333333;
        p.amountIn = 1_000_000_000_000_000_000;
        p.deadline = 1_790_000_000;
        p.leg = DexCall(0x4444444444444444444444444444444444444444, hex"deadbeef01");
        p.legMinOut = 2_500_000_000;
        p.feeBps = 5;
        p.maxGasFee = 50_000;
        p.rail = Rail.Eco;
        p.dstChainId = 42161;
        p.maxFee = 1234;
        p.minFinality = 1000;
        p.eco = EcoTerms({
            salt: bytes32(uint256(0x5555555555555555555555555555555555555555555555555555555555555555)),
            routeDeadline: 1_790_001_000,
            rewardDeadline: 1_790_001_630,
            amount: 2_490_000_000,
            amountOut: 2_489_000_000
        });
        p.dst = Delivery({
            action: Action.Swap,
            recipient: bytes32(uint256(uint160(0x6666666666666666666666666666666666666666))),
            tokenOut: 0x7777777777777777777777777777777777777777,
            minOut: 999,
            principal: bytes32(uint256(0x8888888888888888888888888888888888888888888888888888888888888888)),
            subaccount: bytes32(uint256(0x9999999999999999999999999999999999999999999999999999999999999999)),
            deadline: 1_790_003_600,
            swap: DexCall(0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa, hex"cafe")
        });
    }

    function _types() internal pure returns (string memory) {
        return string.concat(
            '"types":{',
            '"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"SwapParams":[{"name":"ref","type":"bytes32"},{"name":"payer","type":"address"},{"name":"tokenIn","type":"address"},{"name":"amountIn","type":"uint256"},{"name":"deadline","type":"uint256"},{"name":"leg","type":"DexCall"},{"name":"legMinOut","type":"uint256"},{"name":"feeBps","type":"uint256"},{"name":"maxGasFee","type":"uint256"},{"name":"rail","type":"uint8"},{"name":"dstChainId","type":"uint64"},{"name":"maxFee","type":"uint256"},{"name":"minFinality","type":"uint32"},{"name":"eco","type":"EcoTerms"},{"name":"dst","type":"Delivery"}],',
            '"DexCall":[{"name":"target","type":"address"},{"name":"data","type":"bytes"}],',
            '"EcoTerms":[{"name":"salt","type":"bytes32"},{"name":"routeDeadline","type":"uint64"},{"name":"rewardDeadline","type":"uint64"},{"name":"amount","type":"uint256"},{"name":"amountOut","type":"uint256"}],',
            '"Delivery":[{"name":"action","type":"uint8"},{"name":"recipient","type":"bytes32"},{"name":"tokenOut","type":"address"},{"name":"minOut","type":"uint256"},{"name":"principal","type":"bytes32"},{"name":"subaccount","type":"bytes32"},{"name":"deadline","type":"uint64"},{"name":"swap","type":"DexCall"}]',
            "}"
        );
    }

    function _message() internal pure returns (string memory) {
        return string.concat(
            '"message":{',
            '"ref":"0x1111111111111111111111111111111111111111111111111111111111111111",',
            '"payer":"0x2222222222222222222222222222222222222222",',
            '"tokenIn":"0x3333333333333333333333333333333333333333",',
            '"amountIn":"1000000000000000000","deadline":"1790000000",',
            '"leg":{"target":"0x4444444444444444444444444444444444444444","data":"0xdeadbeef01"},',
            '"legMinOut":"2500000000","feeBps":"5","maxGasFee":"50000","rail":1,"dstChainId":"42161","maxFee":"1234","minFinality":1000,',
            '"eco":{"salt":"0x5555555555555555555555555555555555555555555555555555555555555555","routeDeadline":"1790001000","rewardDeadline":"1790001630","amount":"2490000000","amountOut":"2489000000"},',
            '"dst":{"action":1,"recipient":"0x0000000000000000000000006666666666666666666666666666666666666666","tokenOut":"0x7777777777777777777777777777777777777777","minOut":"999",',
            '"principal":"0x8888888888888888888888888888888888888888888888888888888888888888","subaccount":"0x9999999999999999999999999999999999999999999999999999999999999999","deadline":"1790003600",',
            '"swap":{"target":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","data":"0xcafe"}}',
            "}"
        );
    }

    function test_quote_hash_is_the_eip712_digest_a_client_computes() public {
        string memory json = string.concat(
            "{",
            _types(),
            ',"primaryType":"SwapParams",',
            '"domain":{"name":"Swapic Router","version":"1","chainId":8453,"verifyingContract":"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},',
            _message(),
            "}"
        );
        bytes32 expected = vm.eip712HashTypedData(json);
        bytes32 got = QuoteHash.quoteHash(_params(), CHAIN, ROUTER);
        assertEq(got, expected, "the on-chain quote hash is the client's EIP-712 digest");
        assertEq(got, GOLDEN_QUOTE_HASH, "pinned");
        console2.log("quoteHash");
        console2.logBytes32(got);
        console2.log("SwapParams typehash");
        console2.logBytes32(QuoteHash.SWAP_PARAMS_TYPEHASH);
        console2.log("QuoteAcceptance typehash");
        console2.logBytes32(QuoteHash.QUOTE_ACCEPTANCE_TYPEHASH);
        console2.log("domain separator");
        console2.logBytes32(QuoteHash.domainSeparator(CHAIN, ROUTER));
    }

    function test_hook_data_golden_bytes_round_trip() public pure {
        HookData.Data memory d = HookData.Data({
            version: 1,
            ref: bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111)),
            action: Action.Swap,
            deadline: 1_790_003_600,
            recipient: 0x6666666666666666666666666666666666666666,
            tokenOut: 0x7777777777777777777777777777777777777777,
            minOut: 999,
            principal: bytes32(uint256(0x8888888888888888888888888888888888888888888888888888888888888888)),
            subaccount: bytes32(uint256(0x9999999999999999999999999999999999999999999999999999999999999999))
        });
        bytes memory golden = bytes.concat(
            hex"0000000000000000000000000000000000000000000000000000000000000001",
            hex"1111111111111111111111111111111111111111111111111111111111111111",
            hex"0000000000000000000000000000000000000000000000000000000000000001",
            hex"000000000000000000000000000000000000000000000000000000006AB14990",
            hex"0000000000000000000000006666666666666666666666666666666666666666",
            hex"0000000000000000000000007777777777777777777777777777777777777777",
            hex"00000000000000000000000000000000000000000000000000000000000003E7",
            hex"8888888888888888888888888888888888888888888888888888888888888888",
            hex"9999999999999999999999999999999999999999999999999999999999999999"
        );
        assertEq(HookData.encode(d), golden, "encoding is the pinned 9 words");
        HookData.Data memory back = HookData.decode(golden);
        assertEq(keccak256(abi.encode(back)), keccak256(abi.encode(d)), "decode round trips");
    }
}

import {EcoIntent} from "../src/libraries/EcoIntent.sol";
import {IEcoPortal} from "../src/interfaces/IEcoPortal.sol";

contract EcoTemplateGoldenTest is AbiGoldenTest {
    function test_eco_template_golden() public pure {
        SwapParams memory p = _params();
        EcoIntent.Destination memory d = EcoIntent.Destination({
            portal: 0xEC000064576f9C95a8623Bc0eff3db6d296ea6df,
            stable: 0xaf88d065e77c8cC2239327C5EDb3A432268e5831,
            executor: 0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC
        });
        // the Eco hook data carries params.ref
        bytes memory hook = HookData.encode(
            HookData.Data({
                version: 1,
                ref: p.ref,
                action: p.dst.action,
                deadline: p.dst.deadline,
                recipient: address(uint160(uint256(p.dst.recipient))),
                tokenOut: p.dst.tokenOut,
                minOut: p.dst.minOut,
                principal: p.dst.principal,
                subaccount: p.dst.subaccount
            })
        );
        (bytes memory route, IEcoPortal.Reward memory reward, bytes32 intentHash) = EcoIntent.compose(
            p, d, 0xec004Ab4870c4e177c66949329dCdb503CE41022, 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913, hook
        );
        console2.log("route bytes");
        console2.logBytes(route);
        console2.log("routeHash");
        console2.logBytes32(keccak256(route));
        console2.log("rewardHash");
        console2.logBytes32(keccak256(abi.encode(reward)));
        console2.log("intentHash");
        console2.logBytes32(intentHash);
    }
}
