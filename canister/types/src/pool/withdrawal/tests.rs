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
        move_fee: TokenAmount::from(1_000_u64),
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

const INTERFACE: &str = include_str!("../../../../golden/interface.json");

/// The interface golden withdrawal, computed with cast outside this crate: the type string,
/// the domain separator, the struct hash and the digest the staker signs. The app and the
/// watcher's `POST /v2/withdraw` are held to the same file.
#[test]
fn the_interface_golden_withdrawal_holds() {
    let golden: serde_json::Value = serde_json::from_str(INTERFACE).unwrap();
    let golden = &golden["withdrawal"];
    let text = |value: &serde_json::Value| value.as_str().unwrap().to_string();
    let word =
        |value: &serde_json::Value| hex::decode(text(value).trim_start_matches("0x")).unwrap();
    let amount = |value: &serde_json::Value| {
        TokenAmount::from_u256(U256::from_str_radix(&text(value), 10).unwrap())
    };
    let value = &golden["value"];
    let request = WithdrawalRequest {
        staker: text(&value["staker"]).parse().unwrap(),
        usdc: amount(&value["usdc"]),
        max_shares: Shares::new(text(&value["maxShares"]).parse().unwrap()),
        move_fee: amount(&value["moveFee"]),
        network_fee: amount(&value["networkFee"]),
        payout: PayoutTerms {
            dst_chain: ChainId::new(value["dstChain"].as_u64().unwrap()),
            recipient: text(&value["recipient"]).parse().unwrap(),
            token_out: text(&value["tokenOut"]).parse().unwrap(),
            min_out: amount(&value["minOut"]),
            deadline: Timestamp::from_secs(value["deadline"].as_u64().unwrap()).unwrap(),
        },
        nonce: value["nonce"].as_u64().unwrap(),
    };
    let chain = ChainId::new(golden["domain"]["chainId"].as_u64().unwrap());
    let swapic: EvmAddress = text(&golden["domain"]["verifyingContract"])
        .parse()
        .unwrap();
    assert_eq!(text(&golden["type_string"]), WITHDRAWAL_TYPE);
    assert_eq!(
        domain(chain, swapic).hash_struct().to_vec(),
        word(&golden["domain_separator"])
    );
    assert_eq!(
        request.message().eip712_hash_struct().to_vec(),
        word(&golden["struct_hash"])
    );
    assert_eq!(
        request.digest(chain, swapic).to_vec(),
        word(&golden["digest"])
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
