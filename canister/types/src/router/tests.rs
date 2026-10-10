use super::*;
use alloy_primitives::{hex, Bytes};

fn b32(byte: u8) -> FixedBytes<32> {
    FixedBytes([byte; 32])
}

fn a20(byte: u8) -> Address {
    Address::from([byte; 20])
}

/// The golden params of the v1 `abi.md` section 2, every field non-zero, with
/// `networkFee = 50000` in place of `maxGasFee` (I-1).
fn golden() -> SwapParams {
    SwapParams {
        r#ref: b32(0x11),
        payer: a20(0x22),
        tokenIn: a20(0x33),
        amountIn: U256::from(1_000_000_000_000_000_000_u128),
        deadline: U256::from(1_790_000_000_u64),
        leg: DexCall {
            target: a20(0x44),
            data: Bytes::from(hex!("deadbeef01").to_vec()),
        },
        legMinOut: U256::from(2_500_000_000_u64),
        feeBps: U256::from(5),
        networkFee: U256::from(50_000),
        rail: RAIL_ECO,
        dstChainId: 42161,
        maxFee: U256::from(1234),
        minFinality: 1000,
        eco: EcoTerms {
            salt: b32(0x55),
            routeDeadline: 1_790_001_000,
            rewardDeadline: 1_790_001_630,
            amount: U256::from(2_490_000_000_u64),
            amountOut: U256::from(2_489_000_000_u64),
        },
        dst: Delivery {
            action: ACTION_SWAP,
            recipient: FixedBytes(hex!(
                "0000000000000000000000006666666666666666666666666666666666666666"
            )),
            tokenOut: a20(0x77),
            minOut: U256::from(999),
            principal: b32(0x88),
            subaccount: b32(0x99),
            deadline: 1_790_003_600,
            swap: DexCall {
                target: a20(0xaa),
                data: Bytes::from(hex!("cafe").to_vec()),
            },
        },
    }
}

/// The separator and the hash were computed with `cast keccak` and `cast abi-encode`,
/// struct by struct, and the same steps reproduce the v1 golden `0x157e...518e`.
#[test]
fn the_quote_hash_is_the_v2_golden() {
    let swapic = EvmAddress::new([0xbb; 20]);
    let chain = ChainId::new(8453);
    assert_eq!(
        domain(chain, swapic).separator().0,
        hex!("00ffe5e2aeb27c47db7fe384bf5106c37912841d0c1514a5a8840eb88e4d16fb")
    );
    assert_eq!(
        quote_hash(&golden(), chain, swapic).into_bytes(),
        hex!("317d5ad2c5dfc7bc39c40ab9fb7f8ebee5a6e54b7fd9b0c979a9b6ce19617d0c")
    );
}

#[test]
fn the_typehashes_are_swapics() {
    assert_eq!(
        alloy_primitives::keccak256(SWAP_PARAMS_TYPE.as_bytes()).0,
        hex!("c3a7b7e518293128b29fa469c8e7e697eceba3dfe70b3d321fcede98d01d02cd")
    );
    let acceptance = QuoteAcceptance {
        quoteHash: b32(1),
        payer: a20(2),
        tokenIn: a20(3),
        amountIn: U256::from(4),
        deadline: U256::from(5),
        dstChainId: 6,
        recipient: b32(7),
        tokenOut: a20(8),
        minOut: U256::from(9),
        networkFee: U256::from(10),
    };
    assert_eq!(
        acceptance.eip712_type_hash().0,
        hex!("750b5efc0e68fd96ba70b7b34979b27fd1156306f132f83241c1aeb7351b823d")
    );
}

#[test]
fn params_round_trip_through_their_abi_tuple() {
    let params = golden();
    assert_eq!(decode_params(&encode_params(&params)), Some(params));
}

/// The v1 `abi.md` section 5 golden: ref 0x11..11, Swap, deadline 1790003600, recipient
/// 0x66..66, tokenOut 0x77..77, minOut 999, principal 0x88..88, subaccount 0x99..99.
#[test]
fn hook_data_reads_and_writes_the_golden() {
    let golden = hex!(
        "0000000000000000000000000000000000000000000000000000000000000001"
        "1111111111111111111111111111111111111111111111111111111111111111"
        "0000000000000000000000000000000000000000000000000000000000000001"
        "000000000000000000000000000000000000000000000000000000006ab14990"
        "0000000000000000000000006666666666666666666666666666666666666666"
        "0000000000000000000000007777777777777777777777777777777777777777"
        "00000000000000000000000000000000000000000000000000000000000003e7"
        "8888888888888888888888888888888888888888888888888888888888888888"
        "9999999999999999999999999999999999999999999999999999999999999999"
    );
    let hook = HookData::parse(&golden).expect("the golden parses");
    assert_eq!(
        hook,
        HookData {
            version: 1,
            reference: [0x11; 32],
            action: ACTION_SWAP,
            deadline: 1_790_003_600,
            recipient: EvmAddress::new([0x66; 20]),
            token_out: EvmAddress::new([0x77; 20]),
            min_out: U256::from(999),
            principal: [0x88; 32],
            subaccount: [0x99; 32],
        }
    );
    assert_eq!(hook.encode(), golden.to_vec());
    assert_eq!(HookData::parse(&golden[..287]), None);
    let mut unknown_action = golden;
    unknown_action[95] = 3;
    assert_eq!(HookData::parse(&unknown_action), None);
    let mut dirty_address = golden;
    dirty_address[4 * 32] = 1;
    assert_eq!(HookData::parse(&dirty_address), None);
}
