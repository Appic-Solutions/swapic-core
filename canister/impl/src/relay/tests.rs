use super::*;
use alloy_primitives::{Address, Bytes};
use types::router::{Delivery, DexCall, EcoTerms, ACTION_SWAP};

fn params(now: u64) -> SwapParams {
    SwapParams {
        r#ref: FixedBytes([1; 32]),
        payer: Address::from([2; 20]),
        tokenIn: Address::from([3; 20]),
        amountIn: U256::from(1_000_000),
        deadline: U256::from(now + 600),
        leg: DexCall {
            target: Address::ZERO,
            data: Bytes::new(),
        },
        legMinOut: U256::from(1_000_000),
        feeBps: U256::ZERO,
        maxGasFee: U256::from(50_000),
        rail: RAIL_CCTP,
        dstChainId: 42161,
        maxFee: U256::from(100),
        minFinality: 1000,
        eco: EcoTerms {
            salt: FixedBytes::ZERO,
            routeDeadline: 0,
            rewardDeadline: 0,
            amount: U256::ZERO,
            amountOut: U256::ZERO,
        },
        dst: Delivery {
            action: ACTION_SWAP,
            recipient: router::recipient_word(EvmAddress::new([6; 20])),
            tokenOut: Address::from([7; 20]),
            minOut: U256::from(1),
            principal: FixedBytes::ZERO,
            subaccount: FixedBytes::ZERO,
            deadline: now + 3_600,
            swap: DexCall {
                target: Address::ZERO,
                data: Bytes::new(),
            },
        },
    }
}

fn auth(now: u64) -> GaslessAuth {
    GaslessAuth {
        door: DOOR_AUTH_3009,
        gasFee: U256::from(10_000),
        validAfter: U256::ZERO,
        validBefore: U256::from(now + 600),
        nonce: U256::ZERO,
        deadline: U256::ZERO,
        signature: Bytes::from(vec![0; 65]),
        acceptance: Bytes::new(),
    }
}

#[test]
fn a_well_formed_relay_passes_and_each_broken_field_is_refused() {
    let now = 1_800_000_000;
    assert_eq!(check_readable(&params(now), &auth(now), now, 3_600), Ok(()));
    let mut over = auth(now);
    over.gasFee = U256::from(50_001);
    assert!(check_readable(&params(now), &over, now, 3_600).is_err());
    let mut late = params(now);
    late.deadline = U256::from(now);
    assert!(check_readable(&late, &auth(now), now, 3_600).is_err());
    let mut far = params(now);
    far.deadline = U256::from(now + 7_200);
    assert!(check_readable(&far, &auth(now), now, 3_600).is_err());
    let mut not_evm = params(now);
    not_evm.dst.recipient = FixedBytes([9; 32]);
    assert!(check_readable(&not_evm, &auth(now), now, 3_600).is_err());
}
