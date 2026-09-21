use super::*;
use crate::hash::QuoteHash;

const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC_ARBITRUM: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
const ROUTER: &str = "0x2626664c2603336E57B271c5C0b26F421741e481";

fn address(text: &str) -> EvmAddress {
    text.parse().expect("a fixture address parses")
}

fn swap_ref() -> QuoteHash {
    QuoteHash::new([0x11; 32])
}

fn word(byte: u8) -> [u8; 32] {
    [byte; 32]
}

/// Every selector, from `cast sig "<signature>"`. A selector that moves is a call to
/// another function, or to none at all.
#[test]
fn every_selector_is_the_one_cast_computes() {
    for (selector, encoded) in [
        // cast sig "execute(bytes32,(address,uint256,bytes,address,uint256)[],(address,int256)[])"
        ("7a5b699c", vault_execute(swap_ref(), &[], &[])),
        // cast sig "payout(bytes32,address,address,uint256)"
        (
            "40104763",
            vault_payout(
                swap_ref(),
                address(USDC_BASE),
                address(USER),
                TokenAmount::from(1_u8),
            ),
        ),
        // cast sig "refund(bytes32,address,address,uint256)"
        (
            "845b67e8",
            vault_refund(
                swap_ref(),
                address(USDC_BASE),
                address(USER),
                TokenAmount::from(1_u8),
            ),
        ),
        // cast sig "pullWithPermit(bytes32,address,address,uint256,uint256,uint8,bytes32,bytes32)"
        (
            "0263549d",
            vault_pull_with_permit(
                swap_ref(),
                address(USDC_BASE),
                address(USER),
                TokenAmount::from(1_u8),
                UnixSeconds::new(1),
                &Permit {
                    v: 27,
                    r: word(2),
                    s: word(3),
                },
            ),
        ),
        // cast sig "depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32)"
        (
            "8e0250ee",
            cctp_deposit_for_burn(&Burn {
                amount: TokenAmount::from(1_u8),
                destination_domain: 3,
                mint_recipient: address(USER).to_word(),
                burn_token: address(USDC_BASE),
                destination_caller: [0; 32],
                max_fee: TokenAmount::ZERO,
                min_finality_threshold: 1_000,
            }),
        ),
        // cast sig "receiveMessage(bytes,bytes)"
        ("57ecfd28", cctp_receive_message(&[], &[])),
    ] {
        assert_eq!(hex::encode(&encoded[..4]), selector);
    }
}

/// ```text
/// cast calldata "execute(bytes32,(address,uint256,bytes,address,uint256)[],(address,int256)[])" \
///   0x1111111111111111111111111111111111111111111111111111111111111111 \
///   "[(0x2626664c2603336E57B271c5C0b26F421741e481,0,0xdeadbeef,0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913,25000000)]" \
///   "[(0xaf88d065e77c8cC2239327C5EDb3A432268e5831,24900000)]"
/// ```
#[test]
fn an_execute_encodes_as_cast_encodes_it() {
    let calls = [VaultCall {
        target: address(ROUTER),
        value: Wei::ZERO,
        data: vec![0xde, 0xad, 0xbe, 0xef],
        approve_token: address(USDC_BASE),
        approve_amount: TokenAmount::from(25_000_000_u32),
    }];
    let deltas = [VaultDelta {
        token: address(USDC_ARBITRUM),
        min_change: 24_900_000,
    }];
    assert_eq!(
        hex::encode(vault_execute(swap_ref(), &calls, &deltas)),
        concat!(
            "7a5b699c",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "0000000000000000000000000000000000000000000000000000000000000060",
            "0000000000000000000000000000000000000000000000000000000000000180",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000020",
            "0000000000000000000000002626664c2603336e57b271c5c0b26f421741e481",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "00000000000000000000000000000000000000000000000000000000000000a0",
            "000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "00000000000000000000000000000000000000000000000000000000017d7840",
            "0000000000000000000000000000000000000000000000000000000000000004",
            "deadbeef00000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "000000000000000000000000af88d065e77c8cc2239327c5edb3a432268e5831",
            "00000000000000000000000000000000000000000000000000000000017bf1a0",
        )
    );
}

/// ```text
/// cast calldata "payout(bytes32,address,address,uint256)" 0x1111..11 \
///   0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 \
///   0x7551A66653f9a20979ed81835a0b7008EC83401b 24990000
/// ```
#[test]
fn a_payout_encodes_as_cast_encodes_it() {
    assert_eq!(
        hex::encode(vault_payout(
            swap_ref(),
            address(USDC_BASE),
            address(USER),
            TokenAmount::from(24_990_000_u32)
        )),
        concat!(
            "40104763",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b",
            "00000000000000000000000000000000000000000000000000000000017d5130",
        )
    );
}

/// ```text
/// cast calldata "refund(bytes32,address,address,uint256)" 0x1111..11 \
///   0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 \
///   0x7551A66653f9a20979ed81835a0b7008EC83401b 25000000
/// ```
#[test]
fn a_refund_encodes_as_cast_encodes_it() {
    assert_eq!(
        hex::encode(vault_refund(
            swap_ref(),
            address(USDC_BASE),
            address(USER),
            TokenAmount::from(25_000_000_u32)
        )),
        concat!(
            "845b67e8",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b",
            "00000000000000000000000000000000000000000000000000000000017d7840",
        )
    );
}

/// ```text
/// cast calldata "pullWithPermit(bytes32,address,address,uint256,uint256,uint8,bytes32,bytes32)" \
///   0x1111..11 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 \
///   0x7551A66653f9a20979ed81835a0b7008EC83401b 25000000 1800000000 28 \
///   0x2222..22 0x3333..33
/// ```
#[test]
fn a_gasless_pull_encodes_as_cast_encodes_it() {
    assert_eq!(
        hex::encode(vault_pull_with_permit(
            swap_ref(),
            address(USDC_BASE),
            address(USER),
            TokenAmount::from(25_000_000_u32),
            UnixSeconds::new(1_800_000_000),
            &Permit {
                v: 28,
                r: word(0x22),
                s: word(0x33),
            },
        )),
        concat!(
            "0263549d",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b",
            "00000000000000000000000000000000000000000000000000000000017d7840",
            "000000000000000000000000000000000000000000000000000000006b49d200",
            "000000000000000000000000000000000000000000000000000000000000001c",
            "2222222222222222222222222222222222222222222222222222222222222222",
            "3333333333333333333333333333333333333333333333333333333333333333",
        )
    );
}

/// ```text
/// cast calldata "depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32)" \
///   25000000 3 0x0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b \
///   0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 \
///   0x0000000000000000000000000000000000000000000000000000000000000000 3250 1000
/// ```
#[test]
fn a_cctp_burn_encodes_as_cast_encodes_it() {
    assert_eq!(
        hex::encode(cctp_deposit_for_burn(&Burn {
            amount: TokenAmount::from(25_000_000_u32),
            destination_domain: 3,
            mint_recipient: address(USER).to_word(),
            burn_token: address(USDC_BASE),
            destination_caller: [0; 32],
            max_fee: TokenAmount::from(3_250_u32),
            min_finality_threshold: 1_000,
        })),
        concat!(
            "8e0250ee",
            "00000000000000000000000000000000000000000000000000000000017d7840",
            "0000000000000000000000000000000000000000000000000000000000000003",
            "0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b",
            "000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000cb2",
            "00000000000000000000000000000000000000000000000000000000000003e8",
        )
    );
}

/// ```text
/// cast calldata "receiveMessage(bytes,bytes)" 0xdeadbeef 0xc0ffee
/// ```
#[test]
fn a_cctp_mint_encodes_as_cast_encodes_it() {
    assert_eq!(
        hex::encode(cctp_receive_message(
            &[0xde, 0xad, 0xbe, 0xef],
            &[0xc0, 0xff, 0xee]
        )),
        concat!(
            "57ecfd28",
            "0000000000000000000000000000000000000000000000000000000000000040",
            "0000000000000000000000000000000000000000000000000000000000000080",
            "0000000000000000000000000000000000000000000000000000000000000004",
            "deadbeef00000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000003",
            "c0ffee0000000000000000000000000000000000000000000000000000000000",
        )
    );
}

/// An amount above 256 bits cannot be encoded, and the builders take amounts that are
/// already 256-bit values, so the widest amount there is still encodes.
#[test]
fn the_widest_amount_still_encodes() {
    let encoded = vault_payout(
        swap_ref(),
        address(USDC_BASE),
        address(USER),
        TokenAmount::MAX,
    );
    assert_eq!(
        hex::encode(&encoded[encoded.len() - 32..]),
        "f".repeat(64),
        "the amount word is all ones"
    );
}
