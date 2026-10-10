use super::*;
use alloy_primitives::{hex, Bytes};

fn b32(byte: u8) -> FixedBytes<32> {
    FixedBytes([byte; 32])
}

fn a20(byte: u8) -> Address {
    Address::from([byte; 20])
}

/// The golden params of `abi.md` section 2, every field non-zero.
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
        maxGasFee: U256::from(50_000),
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

#[test]
fn the_quote_hash_is_the_routers_golden() {
    let router = EvmAddress::new([0xbb; 20]);
    let chain = ChainId::new(8453);
    assert_eq!(
        router_domain(chain, router).separator().0,
        hex!("d991bab01dd606ea038da0200980b1271f9cf14f0171bca187a82de5ed6148b6")
    );
    assert_eq!(
        quote_hash(&golden(), chain, router).into_bytes(),
        hex!("157e2444cb085aade4fca1374efc685c1ab6447c39f19d12e41c5af8ce30518e")
    );
}

#[test]
fn the_typehashes_are_the_routers() {
    assert_eq!(
        alloy_primitives::keccak256(SWAP_PARAMS_TYPE.as_bytes()).0,
        hex!("e1ce7532432935de5a8a033882f1280e5fa9429be54a090d9319ee2b18be1a5a")
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
        maxGasFee: U256::from(10),
    };
    assert_eq!(
        acceptance.eip712_type_hash().0,
        hex!("f6743e7cc31f15e758f93cb69df8d4aad496bd1c0ee52e2460ef62370c1f0c88")
    );
}

#[test]
fn params_round_trip_through_their_abi_tuple() {
    let params = golden();
    assert_eq!(decode_params(&encode_params(&params)), Some(params));
}

