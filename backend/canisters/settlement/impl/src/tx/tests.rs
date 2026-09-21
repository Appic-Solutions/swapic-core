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
        let priced = fees(ChainId::BASE, at(105)).expect("a fresh reading prices a send");
        assert_eq!(priced.max_fee(), WeiPerGas::from(2_100_000_000_u64));
        assert_eq!(priced.max_priority_fee(), WeiPerGas::from(100_000_000_u64));
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
                chain_id: ChainId::BASE,
                ceiling: MAX_FEE_PER_GAS,
            })
        );
    });
}

/// A fee no chain ever asks is a reading this canister did not compute, and it prices
/// nothing: the watcher is a separate principal from the controller on purpose, and the
/// tip a transaction offers is paid to the block producer in full.
#[test]
fn a_fee_above_the_absolute_ceiling_prices_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        // a thousand times the real tip, which is the shape a compromised watcher pushes
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(1_000_000_000_000_u64),
            },
            at(100),
        );
        assert_eq!(
            fees(ChainId::BASE, at(100)),
            Err(TxError::FeeOutOfRange {
                chain_id: ChainId::BASE,
                ceiling: MAX_FEE_PER_GAS,
            }),
            "the ceiling bounds the tip as well as the ceiling it is paid out of"
        );
    });
}

/// The price is bounded per unit of gas and the bill is bounded as a whole, so a gas limit
/// nobody checked cannot turn a sane price into one this canister would never pay.
#[test]
fn a_transaction_whose_gas_would_cost_more_than_the_bound_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                // a hundred gwei, inside the per-gas ceiling
                base_fee: WeiPerGas::from(50_000_000_000_u64),
                priority_fee: WeiPerGas::from(1_000_000_000_u64),
            },
            at(100),
        );
        let priced = fees(ChainId::BASE, at(100)).expect("a hundred gwei is a real price");
        assert_eq!(
            affordable(ChainId::BASE, priced, GasAmount::from(120_000_u32)),
            Ok(()),
            "a payout at a hundred gwei is twelve thousandths of an ether"
        );
        let absurd = GasAmount::from(100_000_000_u64);
        assert!(
            matches!(
                affordable(ChainId::BASE, priced, absurd),
                Err(TxError::GasCostTooHigh { .. })
            ),
            "a hundred million gas at a hundred gwei is ten ether"
        );
    });
}

/// A replacement doubles what it replaces, never bids below what the chain is asking, and
/// never bids above eight times the going rate. Without the ceiling the doubling is
/// unbounded: fifteen stuck windows is thirty thousand times the original fee, paid to
/// whoever mines it.
#[test]
fn a_replacement_doubles_its_fee_up_to_a_ceiling_and_then_stops() {
    let reading = ChainReading {
        block: BlockNumber::new(19_000_000),
        base_fee: WeiPerGas::from(1_000_000_000_u64),
        priority_fee: WeiPerGas::from(100_000_000_u64),
    }
    .pushed_at(at(100));
    let floor = reading.fees().expect("the reading prices a send");
    let ceiling = reading.fee_ceiling();
    assert_eq!(
        ceiling.max_fee(),
        WeiPerGas::from(8_800_000_000_u64),
        "eight times the base fee plus the tip"
    );

    let mut fees = floor;
    let mut bids = Vec::new();
    while let Some(next) = fees.bumped(floor, ceiling) {
        assert!(
            next.max_fee() > fees.max_fee(),
            "a bid that is not higher is no replacement"
        );
        fees = next;
        bids.push(fees.max_fee());
        assert!(bids.len() < 10, "the ceiling has to stop this");
    }
    assert_eq!(
        bids,
        vec![
            WeiPerGas::from(4_200_000_000_u64),
            WeiPerGas::from(8_400_000_000_u64),
            WeiPerGas::from(8_800_000_000_u64),
        ],
        "double, double, then the ceiling"
    );
    assert_eq!(fees.max_fee(), ceiling.max_fee());
    assert_eq!(
        fees.bumped(floor, ceiling),
        None,
        "at the ceiling there is nothing left to bid"
    );
}

/// The tip a transaction offers is never above the ceiling it is paid out of: under
/// EIP-1559 the producer keeps min(tip, ceiling - base fee), so a tip above the ceiling is
/// not an error the chain reports, it is a number that quietly means something else.
#[test]
fn a_tip_is_never_above_the_ceiling_it_is_paid_out_of() {
    let fees = Fees::new(
        WeiPerGas::from(1_000_000_000_u64),
        WeiPerGas::from(9_000_000_000_u64),
    )
    .expect("both are inside the absolute ceiling");
    assert_eq!(fees.max_priority_fee(), fees.max_fee());
    assert_eq!(
        Fees::new(MAX_FEE_PER_GAS, WeiPerGas::ONE),
        Fees::new(MAX_FEE_PER_GAS, WeiPerGas::ONE),
        "the absolute ceiling itself is allowed"
    );
    for above in [
        Fees::new(
            MAX_FEE_PER_GAS.checked_add(WeiPerGas::ONE).unwrap(),
            WeiPerGas::ONE,
        ),
        Fees::new(
            WeiPerGas::ONE,
            MAX_FEE_PER_GAS.checked_add(WeiPerGas::ONE).unwrap(),
        ),
    ] {
        assert_eq!(above, None, "one wei per gas above the ceiling is above it");
    }
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

/// One of this entry's own hashes, as the receipt reader is given them.
fn ours() -> Vec<TxHash> {
    vec![TxHash::new([0x22; 32])]
}

/// A receipt shaped the way a provider answers one, for `hash`.
fn a_receipt(hash: [u8; 32], status: &str) -> serde_json::Value {
    json!({
        "transactionHash": format!("0x{}", hex::encode(hash)),
        "blockHash": "0x3333333333333333333333333333333333333333333333333333333333333333",
        "blockNumber": "0x121eac0",
        "status": status,
    })
}

/// A receipt is read for the three things a decision needs: which transaction it is, where
/// it landed, and whether it did what it was sent to do.
#[test]
fn a_receipt_reads_its_block_its_hash_and_whether_it_reverted() {
    let read = parse_receipt(&a_receipt([0x22; 32], "0x1"), &ours()).expect("a receipt reads");
    assert_eq!(read.tx_hash, TxHash::new([0x22; 32]));
    assert_eq!(read.block, BlockNumber::new(19_000_000));
    assert!(read.success);

    assert!(
        !parse_receipt(&a_receipt([0x22; 32], "0x0"), &ours())
            .expect("a reverted receipt reads")
            .success
    );
}

/// A receipt decides nothing unless it is about a transaction this entry broadcast. One
/// unreplicated provider supplies both the receipt and the height it is measured against,
/// so an answer about any other transaction must not be able to close an attempt, whatever
/// it says.
#[test]
fn a_receipt_for_a_transaction_we_never_broadcast_is_not_ours() {
    for foreign in [[0x23_u8; 32], [0x00; 32], [0xff; 32]] {
        for status in ["0x1", "0x0"] {
            assert_eq!(
                parse_receipt(&a_receipt(foreign, status), &ours()),
                Err(NotOurReceipt::AnotherTransaction),
                "{}",
                hex::encode(foreign)
            );
        }
    }
    // and every hash this nonce has ever carried is ours, because any of them may land
    let every = vec![TxHash::new([0x22; 32]), TxHash::new([0x23; 32])];
    assert!(parse_receipt(&a_receipt([0x23; 32], "0x1"), &every).is_ok());
}

/// A transaction a provider has not mined answers `null`, which is not a receipt, and
/// neither is a half-written one, nor one that names no block it is in.
#[test]
fn an_unmined_transaction_has_no_receipt_to_read() {
    let full = "0x2222222222222222222222222222222222222222222222222222222222222222";
    let block_hash = "0x3333333333333333333333333333333333333333333333333333333333333333";
    for not_a_receipt in [
        json!(null),
        json!({"transactionHash": "0x22", "blockHash": block_hash, "blockNumber": "0x1", "status": "0x1"}),
        json!({"blockHash": block_hash, "blockNumber": "0x1", "status": "0x1"}),
        json!({"transactionHash": full, "blockHash": block_hash, "status": "0x1"}),
        json!({"transactionHash": full, "blockHash": block_hash, "blockNumber": "not hex", "status": "0x1"}),
        json!({"transactionHash": full, "blockNumber": "0x1", "status": "0x1"}),
        json!({"transactionHash": full, "blockHash": "0x33", "blockNumber": "0x1", "status": "0x1"}),
    ] {
        assert_eq!(
            parse_receipt(&not_a_receipt, &ours()),
            Err(NotOurReceipt::Unmined),
            "{not_a_receipt}"
        );
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

/// An entry in the outbox with `hashes` transactions broadcast at its nonce.
fn entry_with(nonce: u64, hashes: usize) -> OutboxEntry {
    OutboxEntry {
        purpose: TxPurpose::Payout(QuoteHash::new([1; 32])),
        chain_id: ChainId::BASE,
        nonce: types::Nonce::new(nonce),
        attempt: Some(Attempt::FIRST),
        hashes: (0..hashes)
            .map(|i| TxHash::new([i as u8; 32]))
            .collect::<Vec<_>>(),
        raw_tx: vec![0x02],
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        status: OutboxStatus::Sent,
        created_at: at(1),
        last_sent_at: Some(at(1)),
        first_sent_at: Some(at(1)),
        to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
            .parse()
            .unwrap(),
        value: types::Wei::ZERO,
        data: vec![],
        gas_limit: GasAmount::from(120_000_u32),
    }
}

/// The receipts a pass asks for are split so one outcall holds the answer, and an entry
/// never straddles two calls: the reader slices each entry's receipts out of one answer in
/// call order, so a split through an entry would read another entry's receipts as its own.
#[test]
fn the_receipt_batch_is_split_so_one_outcall_holds_the_answer() {
    let one_each: Vec<OutboxEntry> = (0..RECEIPTS_PER_CALL as u64 + 1)
        .map(|nonce| entry_with(nonce, 1))
        .collect();
    let chunks = receipt_chunks(one_each);
    assert_eq!(chunks.len(), 2, "one over the limit is two calls");
    assert_eq!(chunks[0].len(), RECEIPTS_PER_CALL);
    assert_eq!(chunks[1].len(), 1);

    // an entry that has been replaced carries several hashes, and all of them are looked
    // up in the same call as each other
    let replaced: Vec<OutboxEntry> = (0..8).map(|nonce| entry_with(nonce, 5)).collect();
    let chunks = receipt_chunks(replaced);
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![6, 2],
        "six entries of five hashes fill a call of thirty-two"
    );
    for chunk in &chunks {
        let receipts: usize = chunk.iter().map(|entry| entry.hashes.len()).sum();
        assert!(
            receipts <= RECEIPTS_PER_CALL,
            "{receipts} receipts in one call"
        );
    }

    // an entry longer than a whole call goes alone rather than being cut in half
    let long = receipt_chunks(vec![entry_with(0, RECEIPTS_PER_CALL + 4), entry_with(1, 1)]);
    assert_eq!(long.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1]);

    assert!(receipt_chunks(vec![]).is_empty(), "no entries, no calls");
}

/// Both caps are the batch's own and not a constant the batch has to fit inside: a fixed
/// cap is a cliff, because one batch above it is rejected by the system, every later pass
/// builds the same batch, and nothing on that chain ever closes again.
#[test]
fn the_outcall_caps_are_derived_from_the_batch() {
    assert_eq!(send_cap(1), MAX_SEND_BYTES_PER_ITEM);
    assert_eq!(
        send_cap(types::config::MAX_BATCH_ITEMS as usize),
        MAX_SEND_BYTES_PER_ITEM * u64::from(types::config::MAX_BATCH_ITEMS),
        "even the largest batch a config may ask for reserves room for its own answer"
    );
    assert_eq!(
        receipt_cap(1),
        MAX_BLOCK_NUMBER_BYTES + MAX_RECEIPT_BYTES_PER_ITEM
    );
    assert_eq!(
        receipt_cap(RECEIPTS_PER_CALL),
        MAX_BLOCK_NUMBER_BYTES + MAX_RECEIPT_BYTES_PER_ITEM * RECEIPTS_PER_CALL as u64
    );
    assert!(
        receipt_cap(RECEIPTS_PER_CALL) < 2 * 1024 * 1024,
        "a full call stays inside what one outcall may answer"
    );
}
