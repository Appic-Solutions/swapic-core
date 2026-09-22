use super::*;
use serde_json::json;
use types::BlockDepth;

const VAULT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

fn vault() -> EvmAddress {
    VAULT.parse().unwrap()
}

fn quote_hash() -> QuoteHash {
    QuoteHash::new([0x5a; 32])
}

fn word_of(address: &str) -> String {
    let address: EvmAddress = address.parse().unwrap();
    format!("0x{}", hex::encode(address.to_word()))
}

/// A `Deposited` log as a provider answers it, for `quote_hash` at `block`.
fn deposit_log(quote_hash: QuoteHash, block: u64, amount: u64) -> Value {
    json!({
        "address": VAULT.to_ascii_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(deposited_topic())),
            format!("0x{}", hex::encode(quote_hash.as_ref())),
            word_of(USDC),
            word_of(USER),
        ],
        "data": format!("0x{:064x}", amount),
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("0x{}", hex::encode([block as u8; 32])),
        "transactionHash": format!("0x{}", hex::encode([0x77; 32])),
        "logIndex": "0x2",
        "removed": false,
    })
}

/// A valid log decodes to the deposit: the token and the payer out of the indexed words,
/// the amount out of the data, and the transaction and block the chain holds it in.
#[test]
fn a_valid_log_decodes_to_the_deposit() {
    let deposit = decide(
        &[deposit_log(quote_hash(), 19_000_000, 1_000_000)],
        vault(),
        quote_hash(),
        BlockNumber::new(19_000_000),
        BlockDepth::new(1),
    )
    .expect("a deep enough deposit is verified");
    assert_eq!(
        deposit,
        VerifiedDeposit {
            token: USDC.parse().unwrap(),
            from: USER.parse().unwrap(),
            amount: TokenAmount::from(1_000_000_u32),
            tx_ref: TxHash::new([0x77; 32]),
            block: BlockNumber::new(19_000_000),
        }
    );
}

/// A log the head has not reached, or not by the configured depth, is a deposit the
/// chain may still drop: not verified, and the refusal says how deep it is.
#[test]
fn a_log_shallower_than_the_depth_is_not_confirmed() {
    let logs = [deposit_log(quote_hash(), 19_000_005, 1)];
    let shallow = decide(
        &logs,
        vault(),
        quote_hash(),
        BlockNumber::new(19_000_006),
        BlockDepth::new(6),
    );
    assert_eq!(
        shallow,
        Err(DepositError::NotConfirmed {
            block: BlockNumber::new(19_000_005),
            latest: BlockNumber::new(19_000_006),
            depth: BlockDepth::new(6),
        })
    );
    // a block the head has not reached is two moments of the chain, not a confirmation
    let ahead = decide(
        &logs,
        vault(),
        quote_hash(),
        BlockNumber::new(19_000_004),
        BlockDepth::new(1),
    );
    assert!(
        matches!(ahead, Err(DepositError::NotConfirmed { .. })),
        "{ahead:?}"
    );
    // at depth it is verified
    assert!(decide(
        &logs,
        vault(),
        quote_hash(),
        BlockNumber::new(19_000_010),
        BlockDepth::new(6)
    )
    .is_ok());
}

/// No log at all is no deposit, named by chain and quote.
#[test]
fn no_log_is_not_found() {
    assert_eq!(
        decide(
            &[],
            vault(),
            quote_hash(),
            BlockNumber::new(19_000_000),
            BlockDepth::new(1)
        ),
        Err(DepositError::NotFound {
            quote_hash: quote_hash()
        })
    );
}

/// The provider is asked for one quote's deposits, and is not trusted to have listened: a
/// log for another quote, another contract, another event, a removed log or one not in any
/// block decides nothing, and only a log about this quote counts.
#[test]
fn a_log_that_is_not_this_quotes_deposit_is_ignored() {
    let other = deposit_log(QuoteHash::new([0x5b; 32]), 19_000_000, 5);
    let mut elsewhere = deposit_log(quote_hash(), 19_000_000, 5);
    elsewhere["address"] = json!(USDC);
    let mut another_event = deposit_log(quote_hash(), 19_000_000, 5);
    another_event["topics"][0] = json!(format!("0x{}", hex::encode([0xee; 32])));
    let mut removed = deposit_log(quote_hash(), 19_000_000, 5);
    removed["removed"] = json!(true);
    let mut pending = deposit_log(quote_hash(), 19_000_000, 5);
    pending["blockHash"] = Value::Null;
    let mut short_data = deposit_log(quote_hash(), 19_000_000, 5);
    short_data["data"] = json!("0x05");
    let mut three_topics = deposit_log(quote_hash(), 19_000_000, 5);
    three_topics["topics"].as_array_mut().unwrap().pop();
    let noise = vec![
        other,
        elsewhere,
        another_event,
        removed,
        pending,
        short_data,
        three_topics,
        json!(null),
        json!("not a log"),
    ];
    assert_eq!(
        decide(
            &noise,
            vault(),
            quote_hash(),
            BlockNumber::new(19_000_100),
            BlockDepth::new(1)
        ),
        Err(DepositError::NotFound {
            quote_hash: quote_hash()
        }),
        "none of these is this quote's deposit"
    );

    // and the one that is, behind all of them, is found
    let mut with_ours = noise;
    with_ours.push(deposit_log(quote_hash(), 19_000_001, 7));
    let found = decide(
        &with_ours,
        vault(),
        quote_hash(),
        BlockNumber::new(19_000_100),
        BlockDepth::new(1),
    )
    .expect("the deposit behind the noise is verified");
    assert_eq!(found.amount, TokenAmount::from(7_u8));
    assert_eq!(found.block, BlockNumber::new(19_000_001));
}

/// The read is one batch: the head, then the logs of the vault for this quote from the
/// bounded lookback to the head, so the depth is measured against a height from the same
/// answer.
#[test]
fn the_read_asks_for_the_head_and_the_quotes_logs_over_the_lookback() {
    let from = range_from(BlockNumber::new(19_000_000), DepositLookback::new(10_000));
    assert_eq!(from, BlockNumber::new(18_990_000));
    assert_eq!(
        range_from(BlockNumber::new(5), DepositLookback::new(10_000)),
        BlockNumber::new(0),
        "a lookback past genesis starts at genesis"
    );
    let calls = read_calls(vault(), quote_hash(), from);
    assert_eq!(calls[0].0, "eth_blockNumber");
    assert_eq!(calls[0].1, json!([]));
    assert_eq!(calls[1].0, "eth_getLogs");
    assert_eq!(
        calls[1].1,
        json!([{
            "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "topics": [
                "0xcfccc5211684bc31ce945214025a7453ba30a1ffcc38fcb691ce742437f3f256",
                "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
            ],
            "fromBlock": "0x121c3b0",
            "toBlock": "latest",
        }])
    );
    assert_eq!(calls.len(), 2);
}
