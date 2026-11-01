use super::*;
use crate::storage::on_fresh_memory;
use serde_json::json;
use types::chain_data::ChainReading;
use types::Attempt;

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
                ceiling: max_fee_per_gas(ChainId::BASE),
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
                priority_fee: MAX_FEE_PER_GAS,
            },
            at(100),
        );
        assert_eq!(
            fees(ChainId::BASE, at(100)),
            Err(TxError::FeeOutOfRange {
                chain_id: ChainId::BASE,
                ceiling: max_fee_per_gas(ChainId::BASE),
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

/// The price ceiling is per chain: 2,000 gwei is a broken reading on an ETH-priced chain and
/// refused there, and a busy hour on Polygon, priced in POL, where it is paid.
#[test]
fn the_price_ceiling_is_per_chain() {
    on_fresh_memory(|| {
        crate::storage::init();
        let busy = ChainReading {
            block: BlockNumber::new(70_000_000),
            base_fee: WeiPerGas::from(1_000_000_000_000_u64),
            priority_fee: WeiPerGas::from(30_000_000_000_u64),
        };
        chain_data::put(ChainId::BASE, busy, at(100));
        chain_data::put(ChainId::POLYGON, busy, at(100));
        assert_eq!(
            fees(ChainId::BASE, at(100)),
            Err(TxError::FeeOutOfRange {
                chain_id: ChainId::BASE,
                ceiling: WeiPerGas::new(1_000_000_000_000),
            })
        );
        assert!(fees(ChainId::POLYGON, at(100)).is_ok());
    });
}

/// Polygon's native coin is worth a few thousandths of ether, so its bill has its own bound:
/// a CCTP completion at a busy base fee is a real cost there, an absurd gas limit still is not.
#[test]
fn polygon_has_its_own_bound_for_one_transaction() {
    on_fresh_memory(|| {
        crate::storage::init();
        chain_data::put(
            ChainId::POLYGON,
            ChainReading {
                block: BlockNumber::new(70_000_000),
                // a busy hour: 500 gwei
                base_fee: WeiPerGas::from(500_000_000_000_u64),
                priority_fee: WeiPerGas::from(30_000_000_000_u64),
            },
            at(100),
        );
        let priced = fees(ChainId::POLYGON, at(100)).expect("500 gwei is a real Polygon price");
        assert_eq!(
            affordable(ChainId::POLYGON, priced, GasAmount::from(1_185_547_u32)),
            Ok(()),
            "a completion at 500 gwei is about 1.2 POL, above the old 1 POL bound"
        );
        assert!(
            matches!(
                affordable(ChainId::POLYGON, priced, GasAmount::from(100_000_000_u64)),
                Err(TxError::GasCostTooHigh { .. })
            ),
            "a hundred million gas at 500 gwei is over 100 POL"
        );
        assert_eq!(max_transaction_cost(ChainId::BASE), MAX_TRANSACTION_COST);
    });
}

/// Each cancel of a pass is priced at its own instant and not the pass's. The cancels
/// before it each awaited a signature, a round trip of consensus rounds on another subnet,
/// so a `now` read once for the pass is stale by a round trip per cancel: a reading that
/// aged out meanwhile would still look fresh against it, and a reading the watcher pushed
/// meanwhile would be ignored for the older one. Read per cancel, the clock refuses the
/// first and takes the second.
#[test]
fn each_cancel_is_priced_at_its_own_instant() {
    on_fresh_memory(|| {
        crate::storage::init();
        let reading = |base_fee: u64| ChainReading {
            block: BlockNumber::new(19_000_000),
            base_fee: WeiPerGas::from(base_fee),
            priority_fee: WeiPerGas::from(100_000_000_u64),
        };
        chain_data::put(ChainId::BASE, reading(1_000_000_000), at(100));
        let first = cancel_fees(ChainId::BASE, at(105)).expect("the first cancel is priced");
        assert_eq!(first.max_fee(), WeiPerGas::from(2_100_000_000_u64));

        // the first cancel's signature took fifteen seconds: at the second cancel's own
        // instant the reading is past `chain_data_max_age`, and pricing on the pass's
        // instant would have hidden that
        assert_eq!(
            cancel_fees(ChainId::BASE, at(120)),
            Err(TxError::StaleChainData {
                chain_id: ChainId::BASE
            }),
            "the reading the first cancel was priced on has aged out"
        );

        // and a reading pushed while the first was signing is the one the second sees
        chain_data::put(ChainId::BASE, reading(3_000_000_000), at(118));
        let second = cancel_fees(ChainId::BASE, at(120)).expect("the second cancel is priced");
        assert_eq!(
            second.max_fee(),
            WeiPerGas::from(6_100_000_000_u64),
            "priced on the reading that is fresh at its own instant"
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

/// A replacement is bounded by the bill as well as by the price: a large gas limit lowers
/// the price this canister can afford per unit of gas, and the bump clamps to that the way
/// it clamps to the fee ceiling, so the bids keep rising in smaller steps up to the cost
/// cap instead of stopping dead one doubling short of it. Refusing there would leave a
/// transaction underpriced at a bid the bound still had room above.
#[test]
fn a_large_gas_limit_keeps_bumping_in_smaller_steps_up_to_the_cost_cap() {
    on_fresh_memory(|| {
        crate::storage::init();
        // a budget far above the bound, so the bound is what stops the bids
        let roomy = Wei::from(10_000_000_000_000_000_000_u128);
        config::test_set(types::Config {
            evm_chains: types::config::ChainTable(std::collections::BTreeMap::from([(
                ChainId::BASE,
                chain_with_budget(roomy),
            )])),
            ..types::Config::default()
        });
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            },
            at(100),
        );
        // one ether over two hundred million gas is five gwei per gas: above the floor of
        // 2.1 gwei the reading prices at, below the 8.8 gwei the fee ceiling allows
        let gas_limit = GasAmount::from(200_000_000_u64);
        let mut entry = entry_with(0, 1);
        entry.gas_limit = gas_limit;
        entry.max_fee = WeiPerGas::from(2_100_000_000_u64);
        entry.max_priority_fee = WeiPerGas::from(100_000_000_u64);

        let mut bids = Vec::new();
        while let Some(next) = bumped(&entry, ChainId::BASE, at(100)) {
            assert!(
                next.worst_cost(gas_limit).expect("the bill fits") <= MAX_TRANSACTION_COST,
                "every bid stays inside the bound"
            );
            assert!(next.max_fee() > entry.max_fee, "every bid is higher");
            entry.max_fee = next.max_fee();
            entry.max_priority_fee = next.max_priority_fee();
            bids.push(next.max_fee());
            assert!(bids.len() < 10, "the cap has to stop this");
        }
        assert_eq!(
            bids,
            vec![
                WeiPerGas::from(4_200_000_000_u64),
                WeiPerGas::from(5_000_000_000_u64),
            ],
            "double, then the cost cap, then nothing"
        );
        assert_eq!(
            entry.max_fee.transaction_cost(gas_limit),
            Some(MAX_TRANSACTION_COST),
            "the last bid spends exactly the bound"
        );
    });
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
        refusal: None,
        closed_at: None,
        closed_success: None,
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
    assert!(
        send_cap(types::config::MAX_BATCH_ITEMS as usize) < 2 * 1024 * 1024,
        "and so does the largest batch of broadcasts a config may ask for"
    );
}

/// The answers of one chunk are sliced per entry in call order: the head first, then each
/// entry's receipts in the order its hashes were asked for, so an entry that has been
/// replaced is decided on whichever of its own transactions landed, and never on another
/// entry's. An answer with no head, or with fewer answers than calls, decides nothing.
#[test]
fn a_chunks_answers_are_sliced_per_entry_in_call_order() {
    let first = entry_with(0, 1);
    let replaced = entry_with(1, 2);
    let head = Ok(json!("0x121eaca"));
    let landed_second = a_receipt([1; 32], "0x1");
    let answers: Vec<Result<Value, RpcError>> = vec![
        head.clone(),
        Ok(json!(null)),
        Ok(json!(null)),
        Ok(landed_second.clone()),
    ];
    let (latest, per_entry) =
        landed(&[first.clone(), replaced.clone()], &answers, None).expect("a whole answer is read");
    assert_eq!(latest, BlockNumber::new(19_000_010));
    assert_eq!(
        per_entry,
        vec![
            None,
            Some(parse_receipt(&landed_second, &replaced.hashes).expect("its own receipt"))
        ],
        "the first entry has nothing, the second landed its replacement"
    );

    // the same receipt read against the wrong entry is not that entry's
    let crossed: Vec<Result<Value, RpcError>> = vec![
        head.clone(),
        Ok(landed_second),
        Ok(json!(null)),
        Ok(json!(null)),
    ];
    let (_, per_entry) =
        landed(&[first.clone(), replaced.clone()], &crossed, None).expect("a whole answer is read");
    assert_eq!(
        per_entry,
        vec![None, None],
        "a receipt for hash 1 is not the receipt of an entry that never carried it"
    );

    assert_eq!(
        landed(
            std::slice::from_ref(&first),
            &[Ok(json!("not a number")), Ok(json!(null))],
            None
        ),
        None,
        "no head, no depth, nothing to decide"
    );
    assert_eq!(
        landed(&[first, replaced], &[head, Ok(json!(null))], None),
        None,
        "fewer answers than calls is not an answer to this chunk"
    );
}

/// The signature is awaited across consensus rounds on the signing subnet, so by the time
/// it comes back the number it was asked for may have been cancelled and the swap handed
/// a fresh one. The record the signed bytes belong to is the number they carry, not
/// whatever number the swap holds now: a late signature is refused unless the swap still
/// holds exactly the nonce that was allocated for it, so it can never be written at the
/// cancel's nonce or spend the fresh allocation.
#[test]
fn a_late_signature_is_refused_unless_the_swap_still_holds_its_own_nonce() {
    use crate::state::transitions::apply_state_transition;
    use crate::state::transitions::tests::relay_key;
    use crate::state::{MemoryStore, State};
    use types::events::EventType;
    use types::{Event, Nonce, NonceKey, TxHash};

    let qh = relay_key(1);
    let mut state = State::<MemoryStore>::default();
    let apply = |state: &mut State<MemoryStore>, payload: EventType| {
        state.check(&payload).expect("the fold admits it");
        let meta = state.meta();
        let event = Event::seal(
            meta.next_event_index,
            Timestamp::from_nanos(1),
            meta.last_event_hash,
            payload,
        )
        .expect("the payload has a preimage");
        apply_state_transition(state, &event);
    };
    let allocation = |nonce: u64| EventType::TxCreated {
        purpose: TxPurpose::Relay(qh),
        chain_id: ChainId::BASE,
        nonce: Nonce::new(nonce),
        to: EvmAddress::ZERO,
        value: Wei::ZERO,
        data: vec![],
        gas_limit: GasAmount::from(21_000_u32),
        max_fee: WeiPerGas::from(2_u32),
        max_priority_fee: WeiPerGas::from(1_u32),
        action: None,
    };
    let key = |nonce: u64| NonceKey {
        chain_id: ChainId::BASE,
        nonce: Nonce::new(nonce),
    };

    apply(&mut state, allocation(0));
    assert_eq!(
        still_holds(&state, &qh, key(0)),
        Ok(()),
        "the number just allocated is the number the signature is for"
    );

    // the pass cancels number 0 while the signature is on its way, and the swap is handed
    // number 1 by a retry: the late signature carries 0, and 0 is sealed
    apply(
        &mut state,
        EventType::TxCancelled {
            chain_id: ChainId::BASE,
            nonce: Nonce::new(0),
            tx_hash: TxHash::new([1; 32]),
            raw_tx: vec![0x02],
        },
    );
    apply(&mut state, allocation(1));
    assert_eq!(
        still_holds(&state, &qh, key(0)),
        Err(TxError::NonceCancelled {
            quote_hash: qh,
            chain_id: ChainId::BASE,
            nonce: Nonce::new(0),
        }),
        "the swap holds 1 now, and a record carrying 0 must not spend it"
    );
    assert_eq!(
        still_holds(&state, &qh, key(1)),
        Ok(()),
        "the fresh number's own signature is still welcome"
    );
}

/// A depth decides money on both sides, so a chain the deploy listed no depth for decides
/// nothing: the read that would close an attempt or claim a deposit is refused by name
/// rather than run at a depth nobody chose.
#[test]
fn a_chain_with_no_depth_configured_decides_no_money() {
    let config = types::Config::default();
    assert_eq!(
        confirmations(&config, ChainId::ETHEREUM),
        Ok(BlockDepth::new(1))
    );
    let unlisted = ChainId::new(59_144);
    assert_eq!(
        confirmations(&config, unlisted),
        Err(NoDepth { chain_id: unlisted })
    );
    assert_eq!(
        credit_confirmations(&config, unlisted),
        Err(NoDepth { chain_id: unlisted })
    );
}

/// With no crediting depth set for a chain, crediting reads the swap depth, never below the
/// Ethereum floor.
#[test]
fn crediting_falls_back_to_the_swap_depth_with_the_floor() {
    let mut config = types::Config::default();
    config
        .confirmations
        .insert(ChainId::ETHEREUM, BlockDepth::new(1));
    config.credit_confirmations.0.clear();
    assert_eq!(
        credit_confirmations(&config, ChainId::ETHEREUM),
        Ok(BlockDepth::new(12))
    );
    config
        .confirmations
        .insert(ChainId::new(8453), BlockDepth::new(1));
    assert_eq!(
        credit_confirmations(&config, ChainId::new(8453)),
        Ok(BlockDepth::new(1))
    );
    // a crediting depth of its own is the one read, and on Ethereum still never below the
    // floor, even in a config nobody validated
    config
        .credit_confirmations
        .0
        .insert(ChainId::BASE, BlockDepth::new(3));
    assert_eq!(
        credit_confirmations(&config, ChainId::BASE),
        Ok(BlockDepth::new(3))
    );
    config
        .credit_confirmations
        .0
        .insert(ChainId::ETHEREUM, BlockDepth::new(20));
    assert_eq!(
        credit_confirmations(&config, ChainId::ETHEREUM),
        Ok(BlockDepth::new(20))
    );
    config
        .credit_confirmations
        .0
        .insert(ChainId::ETHEREUM, BlockDepth::new(2));
    assert_eq!(
        credit_confirmations(&config, ChainId::ETHEREUM),
        Ok(BlockDepth::new(12))
    );
    // the swap depth on Ethereum is what the outbox reads, and it stays at 1
    assert_eq!(
        confirmations(&config, ChainId::ETHEREUM),
        Ok(BlockDepth::new(1))
    );
}

fn reading(base_fee: u64, priority_fee: u64) -> ChainData {
    ChainReading {
        block: BlockNumber::new(1),
        base_fee: WeiPerGas::from(base_fee),
        priority_fee: WeiPerGas::from(priority_fee),
    }
    .pushed_at(at(100))
}

/// Review Focus 1: a gas spike between the quote and the signature leaves the
/// transaction unsigned, and a replacement never bids past the budget either.
#[test]
fn gas_rule_refuses_over_budget() {
    let gas = GasAmount::from(100_000_u64);
    let budget = Wei::from(1_000_000_000_000_000_u64);
    // 4 gwei base and a 1 gwei tip: 9 gwei times 100,000 gas is 0.0009 ETH
    let calm = reading(4_000_000_000, 1_000_000_000).fees().unwrap();
    assert_eq!(
        within_budget(ChainId::BASE, GasAction::RelayCctp, budget, calm, gas),
        Ok(())
    );
    // a spike to 10 gwei: 21 gwei is 0.0021 ETH
    let spike = reading(10_000_000_000, 1_000_000_000).fees().unwrap();
    assert_eq!(
        within_budget(ChainId::BASE, GasAction::RelayCctp, budget, spike, gas),
        Err(TxError::OverGasBudget {
            chain_id: ChainId::BASE,
            action: GasAction::RelayCctp,
            cost: Wei::from(2_100_000_000_000_000_u64),
            budget,
        })
    );
    // a chain with no budget for the action signs nothing for it
    let config = types::Config::default();
    assert_eq!(
        budget_for(&config, ChainId::BASE, GasAction::RelayCctp),
        Err(TxError::NoGasBudget {
            chain_id: ChainId::BASE,
            action: GasAction::RelayCctp,
        })
    );
    // and a chain that signs it is held to that action's budget
    let config = types::Config {
        evm_chains: types::config::ChainTable(std::collections::BTreeMap::from([(
            ChainId::BASE,
            chain_with_budget(budget),
        )])),
        ..types::Config::default()
    };
    assert_eq!(
        gas_rule(&config, ChainId::BASE, GasAction::Release, calm, gas),
        Ok(())
    );
    assert!(gas_rule(&config, ChainId::BASE, GasAction::Release, spike, gas).is_err());
    // the replacement of a 9 gwei bid stops at the 10 gwei the budget buys per gas, and
    // the one after it is refused
    let bumped = bump(
        ChainId::BASE,
        calm,
        &reading(4_000_000_000, 1_000_000_000),
        gas,
        Some(budget),
    )
    .expect("there is room under the budget");
    assert_eq!(bumped.max_fee(), WeiPerGas::from(10_000_000_000_u64));
    assert_eq!(
        bump(
            ChainId::BASE,
            bumped,
            &reading(4_000_000_000, 1_000_000_000),
            gas,
            Some(budget)
        ),
        None
    );
}

/// A chain the canister signs on, every action held to `budget`.
fn chain_with_budget(budget: Wei) -> types::config::EvmChain {
    let anywhere = EvmAddress::new([0x11; 20]);
    types::config::EvmChain {
        swapic: anywhere,
        runner: anywhere,
        cctp_receiver: anywhere,
        usdc: anywhere,
        token_messenger: anywhere,
        message_transmitter: anywhere,
        gas_limit: types::config::PerAction::all(GasAmount::from(1_000_000_u64)),
        gas_budget: types::config::PerAction::all(budget),
        min_network_fee: types::config::PerAction::all(types::TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
    }
}

/// Review fix 9: a replacement whose transaction names no gas action in the log (a cancel,
/// or an entry from before the action was recorded) is held to the strictest budget its
/// chain has, never only the one-ether bound every transaction has.
#[test]
fn a_replacement_with_no_action_is_held_to_the_strictest_budget() {
    on_fresh_memory(|| {
        crate::storage::init();
        let mut chain = chain_with_budget(Wei::from(10_000_000_000_000_000_000_u128));
        // 0.0003 ether over 120,000 gas is 2.5 gwei a gas
        chain.gas_budget.release = Wei::from(300_000_000_000_000_u64);
        config::test_set(types::Config {
            evm_chains: types::config::ChainTable(std::collections::BTreeMap::from([(
                ChainId::BASE,
                chain,
            )])),
            ..types::Config::default()
        });
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            },
            at(100),
        );
        let mut entry = entry_with(0, 1);
        entry.max_fee = WeiPerGas::from(2_100_000_000_u64);
        let bid = bumped(&entry, ChainId::BASE, at(100)).expect("there is room to bid");
        assert!(
            bid.max_fee() <= WeiPerGas::from(2_500_000_000_u64),
            "{:?} is above what the strictest budget buys",
            bid.max_fee()
        );
        // a chain the canister signs nothing on bids nothing
        let mut elsewhere = entry.clone();
        elsewhere.chain_id = ChainId::ARBITRUM;
        assert_eq!(bumped(&elsewhere, ChainId::ARBITRUM, at(100)), None);
    });
}

/// Review fix 9: a transaction's gas action is in the log. `TxCreated` records it and its
/// hash covers it, the flow its signed step creates takes it, so a replay rebuilds it, and
/// a replacement is held to that action's budget.
#[test]
fn the_gas_action_is_folded_from_the_log() {
    on_fresh_memory(|| {
        crate::storage::init();
        let key = QuoteHash::new([0x0d; 32]);
        let created = |action| EventType::TxCreated {
            purpose: TxPurpose::Relay(key),
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(0),
            to: EvmAddress::new([0xf2; 20]),
            value: Wei::ZERO,
            data: vec![0xf7, 0xed, 0xe9, 0x48],
            gas_limit: GasAmount::from(120_000_u32),
            max_fee: WeiPerGas::from(2_100_000_000_u64),
            max_priority_fee: WeiPerGas::from(100_000_000_u64),
            action,
        };
        assert_ne!(
            created(Some(GasAction::RelayCctp)).canonical_bytes(),
            created(None).canonical_bytes(),
            "the hash covers the action"
        );
        let at = at(100);
        crate::storage::events::append_event_at(created(Some(GasAction::RelayCctp)), at).unwrap();
        crate::storage::events::append_event_at(
            EventType::Flow {
                quote_hash: key,
                step: types::FlowStep::GaslessRelayed {
                    chain_id: ChainId::BASE,
                    nonce: types::Nonce::new(0),
                    tx_hash: TxHash::new([1; 32]),
                    raw_tx: vec![0x02],
                },
            },
            at,
        )
        .unwrap();
        assert_eq!(
            read_state(|state| state.flow(&key)).and_then(|flow| flow.gas_action),
            Some(GasAction::RelayCctp)
        );

        // the relay's own budget holds its replacement: 2.5 gwei a gas for relay_cctp,
        // every other action roomy
        let mut chain = chain_with_budget(Wei::from(10_000_000_000_000_000_000_u128));
        chain.gas_budget.relay_cctp = Wei::from(300_000_000_000_000_u64);
        config::test_set(types::Config {
            evm_chains: types::config::ChainTable(std::collections::BTreeMap::from([(
                ChainId::BASE,
                chain,
            )])),
            ..types::Config::default()
        });
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            },
            at,
        );
        let mut entry = entry_with(0, 1);
        entry.purpose = TxPurpose::Relay(key);
        entry.max_fee = WeiPerGas::from(2_100_000_000_u64);
        let bid = bumped(&entry, ChainId::BASE, at).expect("there is room to bid");
        assert!(bid.max_fee() <= WeiPerGas::from(2_500_000_000_u64));
    });
}

/// Polls `future` once: the decisions below await nothing on the paths they take.
fn poll_once<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("the decision awaited something"),
    }
}

fn receipt_at(block: u64) -> Receipt {
    Receipt {
        tx_hash: TxHash::new([0; 32]),
        block: BlockNumber::new(block),
        success: true,
        network_fee: types::TokenAmount::ZERO,
        eco_intent: None,
    }
}

/// Review fix: on Ethereum an attempt closes at 1 block, long before its receipt is at the
/// crediting depth of 12. The entry stays, marked closed, so a reorg that drops the
/// transaction finds its bytes to send again and the nonces behind it are not stranded; it
/// is never decided twice, and it leaves at the crediting depth.
#[test]
fn a_closed_entry_keeps_its_bytes_until_the_crediting_depth() {
    on_fresh_memory(|| {
        crate::storage::init();
        let swap = BlockDepth::new(1);
        let credit = BlockDepth::new(12);
        let open = entry_with(7, 1);
        outbox::put(open.clone());
        // its line just landed at the swap depth: kept, with the block it closed at
        settle(
            &open,
            BlockNumber::new(100),
            true,
            BlockNumber::new(100),
            credit,
        );
        let kept = outbox::get(open.key()).expect("kept until the crediting depth");
        assert_eq!(kept.closed_at, Some(BlockNumber::new(100)));
        // and no longer what the pull door sees as still out
        assert_eq!(outbox::find(open.purpose), None);

        let log = || read_state(|state| state.meta().next_event_index);
        let before = log();
        let now = at(1_000);
        let decided = |entry: &OutboxEntry, landed: Option<Receipt>, latest: u64| {
            poll_once(decide(
                entry,
                ChainId::BASE,
                landed,
                BlockNumber::new(latest),
                swap,
                credit,
                now,
            ))
        };
        // deep at the swap depth, short of the crediting one: kept as it is
        assert_eq!(decided(&kept, Some(receipt_at(100)), 105), 0);
        assert_eq!(outbox::get(open.key()), Some(kept.clone()));
        // the receipt is gone, a reorg dropped it: the same bytes go out again
        assert_eq!(decided(&kept, None, 105), 0);
        let again = outbox::get(open.key()).expect("still kept");
        assert_eq!(again.status, OutboxStatus::Queued);
        assert_eq!(again.raw_tx, kept.raw_tx);
        assert_eq!(again.closed_at, Some(BlockNumber::new(100)));
        // at the crediting depth: dropped
        assert_eq!(decided(&again, Some(receipt_at(100)), 111), 0);
        assert_eq!(outbox::get(open.key()), None);
        assert_eq!(log(), before, "a closed entry never appends a second line");

        // a chain that credits at its swap depth drops the entry at once, as before
        outbox::put(open.clone());
        settle(
            &open,
            BlockNumber::new(100),
            true,
            BlockNumber::new(100),
            swap,
        );
        assert_eq!(outbox::get(open.key()), None);
    });
}

/// Security review fix: a closed entry is only ever sent again with its own bytes. A
/// lagging node that answers no receipt must not get it a new signature: its closing line
/// is in the log, and a replacement would append a line for an attempt already closed and
/// put new bytes for it on the chain. A receipt that comes back with the other outcome is
/// reported, appends nothing, and is still not decided again.
#[test]
fn a_closed_entry_is_only_sent_again_with_its_own_bytes() {
    on_fresh_memory(|| {
        crate::storage::init();
        let swap = BlockDepth::new(1);
        let credit = BlockDepth::new(12);
        let open = entry_with(8, 1);
        outbox::put(open.clone());
        settle(
            &open,
            BlockNumber::new(100),
            true,
            BlockNumber::new(100),
            credit,
        );
        let kept = outbox::get(open.key()).expect("kept until the crediting depth");
        assert_eq!(kept.closed_success, Some(true));
        // a fresh reading, so a replacement could be priced if one were tried
        let now = at(1_000);
        chain_data::put(
            ChainId::BASE,
            ChainReading {
                block: BlockNumber::new(105),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            },
            now,
        );
        let log = || read_state(|state| state.meta().next_event_index);
        let before = log();
        let decided = |entry: &OutboxEntry, landed: Option<Receipt>, latest: u64| {
            poll_once(decide(
                entry,
                ChainId::BASE,
                landed,
                BlockNumber::new(latest),
                swap,
                credit,
                now,
            ))
        };

        // no receipt, long past the replacement clock: the same bytes again, and nothing new
        assert!(kept.out_for(now).is_some_and(|out| out >= STUCK_AFTER));
        assert_eq!(decided(&kept, None, 105), 0);
        let again = outbox::get(open.key()).expect("still kept");
        assert_eq!(again.status, OutboxStatus::Queued);
        assert_eq!(again.raw_tx, kept.raw_tx);
        assert_eq!(again.hashes, kept.hashes);
        assert_eq!(log(), before);

        // its receipt comes back reverted after a closed success: reported, nothing appended
        let reverted = Receipt {
            success: false,
            ..receipt_at(101)
        };
        let line = divergence(&kept, &reverted).expect("the other outcome is reported");
        assert!(
            line.contains("chain 8453 nonce 8")
                && line.contains("succeeded at block 100")
                && line.contains("reverted at block 101"),
            "{line}"
        );
        assert_eq!(divergence(&kept, &receipt_at(101)), None);
        assert_eq!(decided(&again, Some(reverted), 105), 0);
        assert_eq!(outbox::get(open.key()), Some(again.clone()));
        // and it still leaves at the crediting depth, never decided again
        assert_eq!(decided(&again, Some(reverted), 112), 0);
        assert_eq!(outbox::get(open.key()), None);
        assert_eq!(log(), before);
    });
}

/// P11b: a pool payout's transaction closes with the payout's own line: paid (with the Eco
/// intent its receipt published), or reverted, its order pinned and never signed again
/// (2026-10-23); no fee is booked for it.
#[test]
fn a_pool_payouts_transaction_closes_with_the_payouts_line() {
    use crate::storage::pool::tests::{sign_payout, waiting_withdrawal};
    use types::pool::PayoutStatus;
    let closed = |landed: Receipt| {
        let staker = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let id = waiting_withdrawal(staker);
        sign_payout(id, 0, 7);
        let entry = OutboxEntry {
            purpose: TxPurpose::PoolPayout(id),
            attempt: None,
            ..entry_with(0, 1)
        };
        outbox::put(entry.clone());
        let decided = poll_once(decide(
            &entry,
            ChainId::BASE,
            Some(landed),
            BlockNumber::new(105),
            BlockDepth::new(1),
            BlockDepth::new(1),
            at(1_000),
        ));
        assert_eq!(decided, 1);
        crate::storage::pool::payout(id).unwrap()
    };
    on_fresh_memory(move || {
        crate::storage::init();
        let payout = closed(Receipt {
            success: false,
            ..receipt_at(100)
        });
        assert_eq!(payout.status, PayoutStatus::Reverted);
        assert!(payout.order.is_some(), "its order pinned");
        // never signed again
        assert!(crate::storage::events::append_event_at(
            crate::storage::pool::tests::created(TxPurpose::PoolPayout(payout.id), 1),
            at(1_001),
        )
        .is_err());
    });
    on_fresh_memory(move || {
        crate::storage::init();
        let payout = closed(Receipt {
            eco_intent: Some([0x5e; 32]),
            ..receipt_at(101)
        });
        // its Eco intent's fill is read later
        assert_eq!(payout.status, PayoutStatus::Delivering);
        assert_eq!(payout.intent, Some([0x5e; 32]));
        assert_eq!(payout.paid_block, Some(BlockNumber::new(101)));
        let books = read_state(|state| state.book(&ChainId::BASE));
        assert_eq!(books.fee_income, types::TokenAmount::ZERO);
    });
}

/// CR-17: a pool payout's Eco intent is the one Eco's Portal published with the canister as
/// its creator, read by the one reader the rescue shares: an `IntentPublished` that any
/// other contract the order calls logs, naming the canister, is no intent of the payout's.
#[test]
fn a_payouts_intent_is_the_one_ecos_portal_published() {
    let mut receipt: Value = serde_json::from_str::<Value>(include_str!(
        "../pool/arrival/samples/eco_source_arbitrum.json"
    ))
    .unwrap()["receipt"]
        .clone();
    let canister: EvmAddress = "0x36D34038F04990a58c068e08eD9754e2E3D9bA53"
        .parse()
        .unwrap();
    let portal: EvmAddress = "0xEC000769A73b70e16f361a442292500b3BCf4A85"
        .parse()
        .unwrap();
    let published = receipt["logs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|log| log["logIndex"] == "0x11")
        .unwrap();
    // the sample's creator is its user: the canister's own payout names the canister
    published["topics"][2] = json!(hex0x(&canister.to_word()));
    let intent = parse_hash32(Some(&published["topics"][1])).unwrap();
    let path = |eco_portal| crate::books::FeePath {
        usdc: "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
            .parse()
            .unwrap(),
        runner: "0x7e345acab79102ac9588c60b9358ec4d79438eee"
            .parse()
            .unwrap(),
        canister,
        eco_portal,
    };
    assert_eq!(eco_intent_in(&receipt, &path(Some(portal))), Some(intent));
    assert_eq!(
        eco_intent_in(&receipt, &path(None)),
        None,
        "no Portal, no intent"
    );
    let elsewhere = receipt["logs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|log| log["logIndex"] == "0x11")
        .unwrap();
    elsewhere["address"] = json!("0x00000000000000000000000000000000000bad01");
    assert_eq!(
        eco_intent_in(&receipt, &path(Some(portal))),
        None,
        "logged by another contract"
    );
}

/// The record a signed transaction is written in is decided before its nonce: a cancel, a
/// retired v1 transaction, and a finish, a pool payout or a pool transfer with no detail are
/// refused there, with nothing allocated; a relay, an approval, a rescue and a move need no
/// detail.
#[test]
fn a_transaction_with_no_record_here_is_refused_before_its_nonce() {
    let key = QuoteHash::new([4; 32]);
    let refused = |purpose, words: &str| {
        assert_eq!(
            record_of(purpose, None).err(),
            Some(TxError::NoRecord(match words {
                "cancel" => "a cancel",
                "v1" => "a retired v1 transaction",
                "finish" => "a finish with no detail",
                "payout" => "a pool payout with no order",
                _ => "a pool transfer with no orphan",
            })),
            "{purpose:?}"
        );
    };
    refused(TxPurpose::Cancel(ChainId::BASE), "cancel");
    for v1 in [
        TxPurpose::Burn(key),
        TxPurpose::Mint(key),
        TxPurpose::Payout(key),
        TxPurpose::Refund(key),
        TxPurpose::Reclaim(key),
        TxPurpose::GaslessPull(key),
        TxPurpose::Continue(key),
    ] {
        refused(v1, "v1");
    }
    refused(TxPurpose::Finish(key), "finish");
    refused(TxPurpose::PoolPayout(1), "payout");
    refused(TxPurpose::PoolTransfer([5; 32]), "transfer");
    for kept in [
        TxPurpose::Relay(key),
        TxPurpose::PoolApprove,
        TxPurpose::EcoRescue([6; 32]),
        TxPurpose::PoolMove(2),
    ] {
        assert!(record_of(kept, None).is_ok(), "{kept:?}");
    }
}

/// Each record carries the signed bytes as the fold's step for its purpose reads them: the
/// key, the chain and the nonce the allocation handed out, the hash and the bytes, and the
/// detail its purpose was handed.
#[test]
fn each_record_is_written_with_its_signed_bytes_and_its_detail() {
    use types::pool::{CreditKey, PinnedOrder, PoolStep};
    use types::FlowStep;
    let key = QuoteHash::new([4; 32]);
    let (chain_id, nonce, tx_hash, raw_tx) = (
        ChainId::BASE,
        Nonce::new(7),
        TxHash::new([8; 32]),
        vec![0x02, 0xf8],
    );
    let line = |purpose, detail| {
        record_of(purpose, detail)
            .unwrap()
            .line(key, chain_id, nonce, tx_hash, raw_tx.clone())
    };
    let pool = |step| EventType::Pool { step };
    assert_eq!(
        line(TxPurpose::Relay(key), None),
        EventType::Flow {
            quote_hash: key,
            step: FlowStep::GaslessRelayed {
                chain_id,
                nonce,
                tx_hash,
                raw_tx: raw_tx.clone(),
            },
        }
    );
    let arrival = [9; 32];
    let source_tx = TxHash::new([10; 32]);
    let action = types::flow::FinishAction::Complete;
    assert_eq!(
        line(
            TxPurpose::Finish(key),
            Some(Detail::Finish {
                action,
                arrival,
                source_tx,
            })
        ),
        EventType::Flow {
            quote_hash: key,
            step: FlowStep::CctpFinishSigned {
                chain_id,
                nonce,
                tx_hash,
                raw_tx: raw_tx.clone(),
                action,
                arrival,
                source_tx: Some(source_tx),
            },
        }
    );
    let order = PinnedOrder {
        order_id: [11; 32],
        order: vec![12],
        gas_limit: 900_000,
        deadline_s: 13,
    };
    assert_eq!(
        line(
            TxPurpose::PoolPayout(3),
            Some(Detail::PoolPayout {
                order: order.clone()
            })
        ),
        pool(PoolStep::PayoutSigned {
            id: 3,
            order,
            chain_id,
            nonce,
            tx_hash,
            raw_tx: raw_tx.clone(),
        })
    );
    let credit = CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([14; 32]),
        log_index: 2,
    };
    let to: EvmAddress = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        .parse()
        .unwrap();
    let sent = types::TokenAmount::from(15_u32);
    assert_eq!(
        line(
            TxPurpose::PoolTransfer([16; 32]),
            Some(Detail::PoolTransfer {
                key: credit,
                to,
                sent
            })
        ),
        pool(PoolStep::OrphanRefundSigned {
            key: credit,
            to,
            sent,
            chain_id,
            nonce,
            tx_hash,
            raw_tx: raw_tx.clone(),
        })
    );
    assert_eq!(
        line(TxPurpose::PoolApprove, None),
        pool(PoolStep::ApproveSigned {
            chain_id,
            nonce,
            tx_hash,
            raw_tx: raw_tx.clone(),
        })
    );
    assert_eq!(
        line(TxPurpose::EcoRescue([17; 32]), None),
        pool(PoolStep::EcoRescueSigned {
            order_id: [17; 32],
            chain_id,
            nonce,
            tx_hash,
            raw_tx: raw_tx.clone(),
        })
    );
    assert_eq!(
        line(TxPurpose::PoolMove(5), None),
        pool(PoolStep::MoveSigned {
            id: 5,
            chain_id,
            nonce,
            tx_hash,
            raw_tx,
        })
    );
}

/// The review (2026-10-24): a purpose with no key (a cancel) is none of the pool's, so
/// `in_flight` answers false for it instead of trapping.
#[test]
fn a_cancel_is_never_a_pool_transaction_in_flight() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert!(!in_flight(TxPurpose::Cancel(ChainId::BASE)));
    });
}
