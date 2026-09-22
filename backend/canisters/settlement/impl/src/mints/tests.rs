use super::*;
use serde_json::json;
use types::abi::mint_and_withdraw_topic;
use types::BlockDepth;

const MESSENGER: &str = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d";
const VAULT: &str = "0x2222222222222222222222222222222222222222";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const OTHER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn tx_hash() -> TxHash {
    TxHash::new([0x42; 32])
}

fn word_of(text: &str) -> String {
    format!("0x{}", hex::encode(address(text).to_word()))
}

/// A `MintAndWithdraw` log as the token messenger emits it and a provider prints it.
fn mint_log(messenger: &str, recipient: &str, token: &str, amount: u64, fee: u64) -> Value {
    json!({
        "address": messenger.to_ascii_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(mint_and_withdraw_topic())),
            word_of(recipient),
            word_of(token),
        ],
        "data": format!("0x{amount:064x}{fee:064x}"),
        "logIndex": "0x3",
        "removed": false,
    })
}

/// A mint's receipt as a provider answers it: mined at `block`, with `logs`.
fn receipt(hash: TxHash, block: u64, status: &str, logs: Vec<Value>) -> Value {
    json!({
        "transactionHash": format!("0x{}", hex::encode(hash.as_ref())),
        "blockHash": format!("0x{}", hex::encode([0x41; 32])),
        "blockNumber": format!("0x{block:x}"),
        "status": status,
        "logs": logs,
    })
}

fn wanted() -> MintOf {
    MintOf {
        tx_hash: tx_hash(),
        messenger: address(MESSENGER),
        recipient: address(VAULT),
        token: address(USDC),
    }
}

/// What the mint delivered is the `amount` of the token messenger's `MintAndWithdraw` log
/// naming the destination vault and its USDC: the burn less the fee Circle kept, which
/// the log carries beside it. Read off the mint's own receipt, at the configured depth.
#[test]
fn a_mint_receipt_says_what_was_delivered_and_what_circle_kept() {
    let logs = vec![
        // the transmitter's and the token's own logs sit beside the one that counts
        json!({"address": OTHER.to_ascii_lowercase(), "topics": [format!("0x{}", hex::encode([0xee; 32]))], "data": "0x"}),
        mint_log(MESSENGER, VAULT, USDC, 24_997_500, 2_500),
    ];
    let minted = decide(
        &receipt(tx_hash(), 19_000_010, "0x1", logs),
        &wanted(),
        BlockNumber::new(19_000_010),
        BlockDepth::new(1),
    )
    .expect("the mint delivered");
    assert_eq!(
        minted,
        Minted {
            amount: TokenAmount::from(24_997_500_u32),
            fee: TokenAmount::from(2_500_u32),
            block: BlockNumber::new(19_000_010),
        }
    );
}

/// A receipt decides nothing unless it is the mint's own, mined, deep enough and
/// successful, and its log is the messenger's, for the vault, in the vault's USDC: a
/// receipt of another transaction, a null, a revert, a shallow block, or a mint to
/// somebody else or of another token, each is refused by name and never a `PaidInStable`.
#[test]
fn a_receipt_that_is_not_this_mints_delivery_is_refused_by_name() {
    let good = mint_log(MESSENGER, VAULT, USDC, 24_997_500, 2_500);
    let at = |receipt: &Value, latest: u64| {
        decide(
            receipt,
            &wanted(),
            BlockNumber::new(latest),
            BlockDepth::new(3),
        )
    };
    assert_eq!(
        at(
            &receipt(
                TxHash::new([0x43; 32]),
                19_000_010,
                "0x1",
                vec![good.clone()]
            ),
            19_000_020
        ),
        Err(MintError::AnotherTransaction {
            wanted: tx_hash(),
            found: TxHash::new([0x43; 32]),
        })
    );
    assert_eq!(at(&Value::Null, 19_000_020), Err(MintError::Unmined));
    assert_eq!(
        at(
            &receipt(tx_hash(), 19_000_010, "0x0", vec![good.clone()]),
            19_000_020
        ),
        Err(MintError::Reverted)
    );
    assert_eq!(
        at(
            &receipt(tx_hash(), 19_000_010, "0x1", vec![good.clone()]),
            19_000_011
        ),
        Err(MintError::NotConfirmed {
            block: BlockNumber::new(19_000_010),
            latest: BlockNumber::new(19_000_011),
            depth: BlockDepth::new(3),
        })
    );
    for (case, log) in [
        ("another contract", mint_log(OTHER, VAULT, USDC, 1, 0)),
        ("another recipient", mint_log(MESSENGER, OTHER, USDC, 1, 0)),
        ("another token", mint_log(MESSENGER, VAULT, OTHER, 1, 0)),
    ] {
        assert_eq!(
            at(
                &receipt(tx_hash(), 19_000_010, "0x1", vec![log]),
                19_000_020
            ),
            Err(MintError::NoMintLog { tx_hash: tx_hash() }),
            "{case}"
        );
    }
    let mut short_data = good.clone();
    short_data["data"] = json!("0x01");
    assert_eq!(
        at(
            &receipt(tx_hash(), 19_000_010, "0x1", vec![short_data]),
            19_000_020
        ),
        Err(MintError::NoMintLog { tx_hash: tx_hash() })
    );
    // and the read is one batch: the head, then the receipt of the mint
    let calls = read_calls(tx_hash());
    assert_eq!(calls[0].0, "eth_blockNumber");
    assert_eq!(calls[1].0, "eth_getTransactionReceipt");
    assert_eq!(
        calls[1].1,
        json!([format!("0x{}", hex::encode([0x42; 32]))])
    );
}
