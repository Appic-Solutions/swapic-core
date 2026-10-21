use super::*;
use crate::numeric::Timestamp;

/// The FIXED example of the interface's EIP-712 withdrawal, shared with the services (P13b).
fn fixed() -> WithdrawalRequest {
    let staker: EvmAddress = "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap();
    WithdrawalRequest {
        staker,
        usdc: TokenAmount::from(5_000_000_u64),
        max_shares: Shares::new(5_000_000_000_000_000_000),
        rebalance_share: TokenAmount::from(1_000_u64),
        network_fee: TokenAmount::from(50_000_u64),
        payout: PayoutTerms {
            dst_chain: ChainId::new(42_161),
            recipient: staker,
            token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
                .parse()
                .unwrap(),
            min_out: TokenAmount::from(4_949_000_u64),
            deadline: Timestamp::from_secs(1_800_000_000).unwrap(),
        },
        nonce: 7,
    }
}

fn swapic() -> EvmAddress {
    "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"
        .parse()
        .unwrap()
}

#[test]
fn the_type_is_the_interfaces() {
    assert_eq!(Withdrawal::eip712_encode_type(), WITHDRAWAL_TYPE);
}

/// Pinned, and compared by the controller with the services' own (P13b): the type hash, the
/// domain separator, the struct hash and the digest of the FIXED example.
#[test]
fn the_fixed_examples_digest_is_pinned() {
    let request = fixed();
    assert_eq!(
        hex::encode(request.message().eip712_type_hash()),
        "72f106ef9e14ef53d5b0fbba8ad321a2ff9a0d05b6463dea2b361135f64867c5"
    );
    assert_eq!(
        hex::encode(domain(ChainId::BASE, swapic()).hash_struct()),
        "63d7136ffd00c7447ef648e2ff0d426fbabdb06ee36d6171ffcf5b18354d131a"
    );
    assert_eq!(
        hex::encode(request.message().eip712_hash_struct()),
        "b1faf660a14c0d0aa41838d838dae9747fd481a9fc31a406c928e72843bc00cb"
    );
    assert_eq!(
        hex::encode(request.digest(ChainId::BASE, swapic())),
        "198b9ff0da5fb38d93be61e50a38be4fca4e611bac9b06097587f11d9768fd6c"
    );
}

#[test]
fn the_deadline_is_signed_in_whole_seconds() {
    let mut request = fixed();
    request.payout.deadline = Timestamp::from_nanos(1_800_000_000_999_999_999);
    assert_eq!(request.message().deadline, 1_800_000_000);
    assert_eq!(
        request.digest(ChainId::BASE, swapic()),
        fixed().digest(ChainId::BASE, swapic())
    );
}
