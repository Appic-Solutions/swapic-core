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

/// The lines that close an attempt carry the attempt the entry recorded and never a
/// default. A cancel closes no attempt and writes no line, and an entry that names a swap
/// but carries no attempt number is left where it is rather than closed against a number
/// this canister invented: the consensus log would otherwise hold an attempt nobody signed
/// for.
#[test]
fn closing_an_entry_never_invents_an_attempt() {
    on_fresh_memory(|| {
        crate::storage::init();
        let never = |_: QuoteHash, _: Attempt| -> EventType {
            panic!("no line is written for an entry that carries no attempt")
        };

        let mut cancel = entry_with(0, 1);
        cancel.purpose = TxPurpose::Cancel(ChainId::BASE);
        cancel.attempt = None;
        outbox::put(cancel.clone());
        close(&cancel, never);
        assert_eq!(
            outbox::get(cancel.key()),
            None,
            "a cancel's receipt closes its entry and writes nothing"
        );

        let mut unnumbered = entry_with(1, 1);
        unnumbered.attempt = None;
        outbox::put(unnumbered.clone());
        close(&unnumbered, never);
        assert_eq!(
            outbox::get(unnumbered.key()),
            Some(unnumbered),
            "a swap's entry with no attempt is left alone, not closed as attempt one"
        );

        // a pull is signed against its quote and not a swap's attempt: its record sealed
        // the number, and the deposit its bytes made is read by the claim, so its receipt
        // closes its entry and writes nothing, like a cancel's
        let mut pull = entry_with(2, 1);
        pull.purpose = TxPurpose::GaslessPull(QuoteHash::new([2; 32]));
        pull.attempt = None;
        outbox::put(pull.clone());
        close(&pull, never);
        assert_eq!(
            outbox::get(pull.key()),
            None,
            "a pull's receipt closes its entry and writes nothing"
        );
    });
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
        landed(&[first.clone(), replaced.clone()], &answers).expect("a whole answer is read");
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
        landed(&[first.clone(), replaced.clone()], &crossed).expect("a whole answer is read");
    assert_eq!(
        per_entry,
        vec![None, None],
        "a receipt for hash 1 is not the receipt of an entry that never carried it"
    );

    assert_eq!(
        landed(
            std::slice::from_ref(&first),
            &[Ok(json!("not a number")), Ok(json!(null))]
        ),
        None,
        "no head, no depth, nothing to decide"
    );
    assert_eq!(
        landed(&[first, replaced], &[head, Ok(json!(null))]),
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
    use crate::state::transitions::tests::{funds, swap_id};
    use crate::state::{MemoryStore, State};
    use types::events::EventType;
    use types::{Event, Nonce, NonceKey, TxHash};

    let qh = swap_id(1);
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
        purpose: TxPurpose::Payout(qh),
        chain_id: ChainId::BASE,
        nonce: Nonce::new(nonce),
        to: EvmAddress::ZERO,
        value: Wei::ZERO,
        data: vec![],
        gas_limit: GasAmount::from(21_000_u32),
        max_fee: WeiPerGas::from(2_u32),
        max_priority_fee: WeiPerGas::from(1_u32),
    };
    let key = |nonce: u64| NonceKey {
        chain_id: ChainId::BASE,
        nonce: Nonce::new(nonce),
    };

    apply(&mut state, funds(1));
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
        Ok(types::config::MIN_ETHEREUM_CONFIRMATIONS)
    );
    let unlisted = ChainId::new(59_144);
    assert_eq!(
        confirmations(&config, unlisted),
        Err(NoDepth { chain_id: unlisted })
    );
}
