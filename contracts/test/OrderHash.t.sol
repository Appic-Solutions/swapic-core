// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import "forge-std/Test.sol";
import "../src/SwapicTypes.sol";
import {OrderHash} from "../src/libraries/OrderHash.sol";
import {Envelope} from "../src/libraries/Envelope.sol";

contract OrderHashHarness {
    function orderId(Order calldata o, uint256 chainId, address swapic) external pure returns (bytes32) {
        return OrderHash.orderId(o, chainId, swapic);
    }

    function acceptanceDigest(Order calldata o, bytes32 id, uint256 chainId, address swapic)
        external
        pure
        returns (bytes32)
    {
        return OrderHash.acceptanceDigest(o, id, chainId, swapic);
    }

    function decode(bytes calldata b) external pure returns (Envelope.Data memory) {
        return Envelope.decode(b);
    }
}

/// The golden order of abi.md: 2 steps, one with 2 writes, a payout.
abstract contract GoldenOrder is Test {
    uint256 constant CHAIN = 8453;
    address constant SWAPIC = 0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB;

    function _golden() internal pure returns (Order memory o) {
        o.ref = bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111));
        o.payer = 0x2222222222222222222222222222222222222222;
        o.tokenIn = 0x3333333333333333333333333333333333333333;
        o.amountIn = 1_000_000_000_000_000_000;
        o.deadline = 1_790_000_000;
        o.steps = new Step[](2);
        o.steps[0] = Step({
            target: 0x4444444444444444444444444444444444444444,
            data: hex"deadbeef01",
            token: 0x3333333333333333333333333333333333333333,
            min: 1_000_000_000_000_000_000,
            writes: new Write[](0)
        });
        Write[] memory ws = new Write[](2);
        ws[0] = Write({at: 4, minus: 0});
        ws[1] = Write({at: 37, minus: 57_115});
        o.steps[1] = Step({
            target: 0x5555555555555555555555555555555555555555,
            data: hex"cafebabe00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001",
            token: 0x6666666666666666666666666666666666666666,
            min: 2_500_000_000,
            writes: ws
        });
        o.payout = Payout({
            token: 0x7777777777777777777777777777777777777777,
            recipient: 0x8888888888888888888888888888888888888888,
            minOut: 999
        });
    }

    function _domain() internal pure returns (string memory) {
        return
        '"domain":{"name":"Swapic","version":"3","chainId":8453,"verifyingContract":"0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}';
    }

    function _orderJson() internal pure returns (string memory) {
        return string.concat(
            "{",
            '"types":{',
            '"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"Order":[{"name":"ref","type":"bytes32"},{"name":"payer","type":"address"},{"name":"tokenIn","type":"address"},{"name":"amountIn","type":"uint256"},{"name":"deadline","type":"uint256"},{"name":"steps","type":"Step[]"},{"name":"payout","type":"Payout"},{"name":"memo","type":"bytes"}],',
            '"Step":[{"name":"target","type":"address"},{"name":"data","type":"bytes"},{"name":"token","type":"address"},{"name":"min","type":"uint256"},{"name":"writes","type":"Write[]"}],',
            '"Write":[{"name":"at","type":"uint32"},{"name":"minus","type":"uint256"}],',
            '"Payout":[{"name":"token","type":"address"},{"name":"recipient","type":"address"},{"name":"minOut","type":"uint256"}]',
            '},"primaryType":"Order",',
            _domain(),
            ',"message":{',
            '"ref":"0x1111111111111111111111111111111111111111111111111111111111111111",',
            '"payer":"0x2222222222222222222222222222222222222222",',
            '"tokenIn":"0x3333333333333333333333333333333333333333",',
            '"amountIn":"1000000000000000000","deadline":"1790000000",',
            '"steps":[',
            '{"target":"0x4444444444444444444444444444444444444444","data":"0xdeadbeef01","token":"0x3333333333333333333333333333333333333333","min":"1000000000000000000","writes":[]},',
            '{"target":"0x5555555555555555555555555555555555555555","data":"0xcafebabe00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001","token":"0x6666666666666666666666666666666666666666","min":"2500000000","writes":[{"at":4,"minus":"0"},{"at":37,"minus":"57115"}]}',
            "],",
            '"payout":{"token":"0x7777777777777777777777777777777777777777","recipient":"0x8888888888888888888888888888888888888888","minOut":"999"},',
            '"memo":"0x"',
            "}}"
        );
    }

    function _acceptanceJson(bytes32 id) internal pure returns (string memory) {
        return string.concat(
            "{",
            '"types":{',
            '"EIP712Domain":[{"name":"name","type":"string"},{"name":"version","type":"string"},{"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}],',
            '"QuoteAcceptance":[{"name":"orderId","type":"bytes32"},{"name":"payer","type":"address"},{"name":"tokenIn","type":"address"},{"name":"amountIn","type":"uint256"},{"name":"deadline","type":"uint256"}]',
            '},"primaryType":"QuoteAcceptance",',
            _domain(),
            ',"message":{',
            '"orderId":"',
            vm.toString(id),
            '",',
            '"payer":"0x2222222222222222222222222222222222222222",',
            '"tokenIn":"0x3333333333333333333333333333333333333333",',
            '"amountIn":"1000000000000000000","deadline":"1790000000"',
            "}}"
        );
    }
}

/// Pinned against foundry's own EIP-712 encoder fed the client JSON
/// (`vm.eip712HashTypedData`), so a wrong type string, nested type order or
/// array encoding cannot pass. Typehashes and the envelope from `cast`.
contract OrderHashTest is GoldenOrder {

    OrderHashHarness h;

    function setUp() public {
        h = new OrderHashHarness();
    }

    function test_order_id_is_the_eip712_digest_a_client_computes() public {
        bytes32 got = h.orderId(_golden(), CHAIN, SWAPIC);
        assertEq(got, vm.eip712HashTypedData(_orderJson()));
    }

    function test_acceptance_digest_is_the_eip712_digest_a_client_computes() public {
        bytes32 id = h.orderId(_golden(), CHAIN, SWAPIC);
        bytes32 got = h.acceptanceDigest(_golden(), id, CHAIN, SWAPIC);
        assertEq(got, vm.eip712HashTypedData(_acceptanceJson(id)));
    }

    function test_every_field_changes_the_id() public {
        bytes32 base = h.orderId(_golden(), CHAIN, SWAPIC);
        Order memory o = _golden();
        o.steps[1].writes[1].at = 36;
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.steps[1].writes[1].minus = 57_116;
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.steps[0].data = hex"deadbeef02";
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.steps[0].min = 1;
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.payout.minOut = 998;
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.amountIn = 1;
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        o = _golden();
        o.memo = hex"01";
        assertTrue(h.orderId(o, CHAIN, SWAPIC) != base);
        assertTrue(h.orderId(_golden(), CHAIN + 1, SWAPIC) != base);
        assertTrue(h.orderId(_golden(), CHAIN, address(1)) != base);
    }

    function test_type_goldens() public pure {
        assertEq(OrderHash.ORDER_TYPEHASH, 0x391c982619bb5233046c55e62c42b9d3e4c1ec09f980e88f048c95402a61e6ec);
        assertEq(OrderHash.STEP_TYPEHASH, 0x0682eb4409dbcbd035b45046e7629810578851f37499e32990aefe0c8dbc2134);
        assertEq(OrderHash.WRITE_TYPEHASH, 0xc4688b5dd66453b7f007d0ec6009b512f5df252efe9072aff82a09a6779744c3);
        assertEq(OrderHash.PAYOUT_TYPEHASH, 0x73644d236b331510ff1d9a455c49a20d6dd646f5a4c449a25ee91053263f8369);
        assertEq(OrderHash.ACCEPTANCE_TYPEHASH, 0x0542f436365880f872dcd8b65071eea3eecb5e112bbdf0f91b3cd9369022c9eb);
    }

    function _envelope() internal pure returns (Envelope.Data memory) {
        return Envelope.Data({
            version: 3,
            ref: bytes32(uint256(0x1111111111111111111111111111111111111111111111111111111111111111)),
            action: Action.Swap,
            deadline: 1_790_003_600,
            recipient: 0x6666666666666666666666666666666666666666,
            tokenOut: 0x7777777777777777777777777777777777777777,
            minOut: 999,
            principal: bytes32(uint256(0x8888888888888888888888888888888888888888888888888888888888888888)),
            subaccount: bytes32(uint256(0x9999999999999999999999999999999999999999999999999999999999999999)),
            minGas: 700_000
        });
    }

    function _goldenEnvelope() internal pure returns (bytes memory) {
        return bytes.concat(
            hex"0000000000000000000000000000000000000000000000000000000000000003",
            hex"1111111111111111111111111111111111111111111111111111111111111111",
            hex"0000000000000000000000000000000000000000000000000000000000000001",
            hex"000000000000000000000000000000000000000000000000000000006ab14990",
            hex"0000000000000000000000006666666666666666666666666666666666666666",
            hex"0000000000000000000000007777777777777777777777777777777777777777",
            hex"00000000000000000000000000000000000000000000000000000000000003e7",
            hex"8888888888888888888888888888888888888888888888888888888888888888",
            hex"9999999999999999999999999999999999999999999999999999999999999999",
            hex"00000000000000000000000000000000000000000000000000000000000aae60"
        );
    }

    function test_envelope_golden_round_trips() public view {
        assertEq(Envelope.encode(_envelope()), _goldenEnvelope());
        assertEq(keccak256(abi.encode(h.decode(_goldenEnvelope()))), keccak256(abi.encode(_envelope())));
    }

    function test_envelope_decode_refuses() public {
        bytes memory b = _goldenEnvelope();
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(bytes.concat(b, hex"00"));

        b[31] = 0x01;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        b = _goldenEnvelope();
        b[95] = 0x03;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        b = _goldenEnvelope();
        b[100] = 0x01;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        b = _goldenEnvelope();
        b[128] = 0x01;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        b = _goldenEnvelope();
        b[160] = 0x01;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        b = _goldenEnvelope();
        b[290] = 0x01;
        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(b);

        vm.expectRevert(Envelope.BadEnvelope.selector);
        h.decode(_slice(_goldenEnvelope(), 288));
    }

    function _slice(bytes memory b, uint256 n) internal pure returns (bytes memory out) {
        out = new bytes(n);
        for (uint256 i; i < n; ++i) {
            out[i] = b[i];
        }
    }
}
