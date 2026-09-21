use super::*;
use crate::storage::on_fresh_memory;
use serde_json::json;
use types::chain_data::ChainReading;

fn at(secs: u64) -> Timestamp {
    Timestamp::from_secs(secs).expect("a test instant is inside the epoch")
}

/// The fee ceiling carries a transaction through a rising base fee, and the tip is the
/// watcher's own: twice the base fee plus the tip, computed on the types that hold the
/// units.
#[test]
fn the_fee_ceiling_leaves_room_for_a_rising_base_fee() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert_eq!(
            fees(ChainId::BASE, at(100)),
            Err(TxError::StaleChainData {
                chain_id: ChainId::BASE
            }),
            "nothing is priced without chain data"
        );
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            },
            at(100),
        );
        assert_eq!(
            fees(ChainId::BASE, at(105)),
            Ok((
                WeiPerGas::from(2_100_000_000_u64),
                WeiPerGas::from(100_000_000_u64)
            ))
        );
        // `chain_data_max_age` is ten seconds by default, and a stale reading prices
        // nothing rather than pricing a transaction wrong
        assert_eq!(
            fees(ChainId::BASE, at(200)),
            Err(TxError::StaleChainData {
                chain_id: ChainId::BASE
            })
        );
    });
}

/// A base fee no 256-bit price holds cannot be doubled, and that is a refusal rather than
/// a wrap.
#[test]
fn a_fee_that_cannot_be_doubled_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(1),
                base_fee: WeiPerGas::MAX,
                priority_fee: WeiPerGas::ONE,
            },
            at(100),
        );
        assert_eq!(
            fees(ChainId::BASE, at(100)),
            Err(TxError::FeeOutOfRange {
                chain_id: ChainId::BASE
            })
        );
    });
}

/// A provider that already holds the transaction has accepted it: the bytes are on the
/// network, which is the whole point of a broadcast, and a nonce is never re-sent because
/// a node was honest about having seen it.
#[test]
fn a_provider_that_already_has_the_transaction_has_accepted_it() {
    for accepted in [
        "already known",
        "ALREADY KNOWN",
        "known transaction: 0xabc",
        "replacement transaction underpriced: already imported",
    ] {
        assert!(already_on_the_network(accepted), "{accepted}");
    }
    for refused in [
        "nonce too low",
        "insufficient funds for gas * price + value",
        "transaction underpriced",
        "",
    ] {
        assert!(!already_on_the_network(refused), "{refused}");
    }
}

/// A nonce the chain calls too low is a nonce already spent, and this canister is the only
/// account that spends it: one of the transactions this entry broadcast at that nonce is
/// mined, so the entry goes to the receipt reader instead of sitting in the queue forever.
#[test]
fn a_nonce_the_chain_calls_too_low_is_a_nonce_already_spent() {
    for spent in [
        "nonce too low",
        "Nonce too low: next nonce 8, tx nonce 7",
        "OldNonce",
    ] {
        assert!(nonce_already_spent(spent), "{spent}");
        assert!(
            !already_on_the_network(spent),
            "a spent nonce is not a transaction in a mempool: {spent}"
        );
    }
    for other in ["already known", "insufficient funds", "underpriced", ""] {
        assert!(!nonce_already_spent(other), "{other}");
    }
}

/// A receipt is read for the three things a decision needs: which transaction it is, where
/// it landed, and whether it did what it was sent to do.
#[test]
fn a_receipt_reads_its_block_its_hash_and_whether_it_reverted() {
    let receipt = json!({
        "transactionHash": "0x2222222222222222222222222222222222222222222222222222222222222222",
        "blockNumber": "0x121eac0",
        "status": "0x1",
    });
    let read = parse_receipt(&receipt).expect("a receipt reads");
    assert_eq!(read.tx_hash, TxHash::new([0x22; 32]));
    assert_eq!(read.block, BlockNumber::new(19_000_000));
    assert!(read.success);

    let reverted = json!({
        "transactionHash": "0x2222222222222222222222222222222222222222222222222222222222222222",
        "blockNumber": "0x121eac0",
        "status": "0x0",
    });
    assert!(
        !parse_receipt(&reverted)
            .expect("a reverted receipt reads")
            .success
    );
}

/// A transaction a provider has not mined answers `null`, which is not a receipt, and
/// neither is a half-written one.
#[test]
fn an_unmined_transaction_has_no_receipt_to_read() {
    for not_a_receipt in [
        json!(null),
        json!({"transactionHash": "0x22", "blockNumber": "0x1", "status": "0x1"}),
        json!({"blockNumber": "0x1", "status": "0x1"}),
        json!({"transactionHash": "0x2222222222222222222222222222222222222222222222222222222222222222", "status": "0x1"}),
        json!({"transactionHash": "0x2222222222222222222222222222222222222222222222222222222222222222", "blockNumber": "not hex", "status": "0x1"}),
    ] {
        assert!(parse_receipt(&not_a_receipt).is_none(), "{not_a_receipt}");
    }
}

/// Block numbers cross the wire as `0x`-prefixed hex, and nothing else is one.
#[test]
fn a_block_number_is_prefixed_hex() {
    assert_eq!(parse_block_number("0x0"), Some(BlockNumber::new(0)));
    assert_eq!(
        parse_block_number("0x121eac0"),
        Some(BlockNumber::new(19_000_000))
    );
    for not_a_number in ["1221e40", "0x", "0xzz", ""] {
        assert_eq!(parse_block_number(not_a_number), None, "{not_a_number}");
    }
}

/// Raw bytes cross the wire prefixed, which is how a chain takes them.
#[test]
fn raw_bytes_cross_the_wire_prefixed() {
    assert_eq!(hex0x(&[0x02, 0xf8, 0x6b]), "0x02f86b");
    assert_eq!(hex0x(&[]), "0x");
}
