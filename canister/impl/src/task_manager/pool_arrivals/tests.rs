use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, funded_pool, usdc};
use crate::test_util::block_on;
use std::cell::Cell;
use types::pool::CreditKey;

#[test]
fn a_read_covers_at_most_five_hundred_blocks_from_its_cursor() {
    let block = BlockNumber::new;
    // no cursor: from the lookback
    assert_eq!(
        range(None, None, block(10_000)),
        Some((block(9_400), block(9_899)))
    );
    // a cursor behind the head: from the block after it
    assert_eq!(
        range(Some(block(9_950)), None, block(10_000)),
        Some((block(9_951), block(10_000)))
    );
    // a cursor inside a block: from that block, where the read goes on
    assert_eq!(
        range(Some(block(9_950)), Some(7), block(10_000)),
        Some((block(9_950), block(10_000)))
    );
    // a cursor older than the lookback: from the lookback
    assert_eq!(
        range(Some(block(1)), None, block(10_000)),
        Some((block(9_400), block(9_899)))
    );
    // nothing new
    assert_eq!(range(Some(block(10_000)), None, block(10_000)), None);
}

/// One transfer log of `eth_getLogs`, of `amount` micro USDC.
fn log(tx: u8, block: u64, index: u64, amount: u64) -> serde_json::Value {
    serde_json::json!({
        "transactionHash": format!("0x{}", hex::encode([tx; 32])),
        "blockNumber": format!("0x{block:x}"),
        "logIndex": format!("0x{index:x}"),
        "data": format!("0x{:064x}", amount),
    })
}

#[test]
fn every_transfer_reads_in_block_and_log_order() {
    let mut removed = log(4, 9, 0, 5_000_000);
    removed["removed"] = serde_json::json!(true);
    let logs = serde_json::json!([log(2, 11, 3, 7), log(1, 10, 5, 2_000_000), removed]);
    assert_eq!(
        logged(&logs),
        vec![
            Logged {
                tx: TxHash::new([1; 32]),
                block: BlockNumber::new(10),
                log_index: 5,
                amount: TokenAmount::from(2_000_000_u32),
            },
            Logged {
                tx: TxHash::new([2; 32]),
                block: BlockNumber::new(11),
                log_index: 3,
                amount: TokenAmount::from(7_u8),
            },
        ]
    );
}

/// Review fix 6, and the cursor rule: a pass reads each transaction once, never one whose
/// every transfer is credited or dust, at most its cap of receipts out of its cap of
/// transfers considered, and is done only through the transfer before the first worth
/// reading that it did not read, inside a block if need be.
#[test]
fn a_pass_reads_what_is_worth_it_up_to_its_cap() {
    let usdc = 1_000_000;
    let logs = serde_json::json!([
        // block 10: one transaction twice, a dust one, a credited one
        log(1, 10, 0, 5 * usdc),
        log(1, 10, 1, 5 * usdc),
        log(2, 10, 2, DUST - 1),
        log(3, 10, 3, 5 * usdc),
        // block 11: two
        log(4, 11, 0, 5 * usdc),
        log(5, 11, 1, DUST),
        // block 12: one
        log(6, 12, 0, 5 * usdc),
    ]);
    let logged = logged(&logs);
    let credited = |key: &CreditKey| key.tx == TxHash::new([3; 32]);
    let at = |block: u64, log: u64| (BlockNumber::new(block), log);
    let tx = |byte: u8, block: u64, log: u64| (TxHash::new([byte; 32]), at(block, log));
    let plan = |receipts: usize, transfers: usize| {
        plan_pass(&logged, ChainId::BASE, credited, receipts, transfers)
    };
    assert_eq!(
        plan(20, 100),
        PassPlan {
            read: vec![tx(1, 10, 0), tx(4, 11, 0), tx(5, 11, 1), tx(6, 12, 0)],
            done: Done::Range,
        }
    );
    // a cap of two: done through block 10's last transfer, and block 11's second is not
    // passed: done through its first
    assert_eq!(
        plan(2, 100),
        PassPlan {
            read: vec![tx(1, 10, 0), tx(4, 11, 0)],
            done: Done::Through(at(11, 0)),
        }
    );
    // five transfers considered: block 11's first is the last of them
    assert_eq!(
        plan(20, 5),
        PassPlan {
            read: vec![tx(1, 10, 0), tx(4, 11, 0)],
            done: Done::Through(at(11, 0)),
        }
    );
    // a cap of one: nothing passed that was not read
    assert_eq!(
        plan(1, 100),
        PassPlan {
            read: vec![tx(1, 10, 0)],
            done: Done::Through(at(10, 3)),
        }
    );
    // a receipt that failed for now holds the read before its first transfer
    assert_eq!(
        done_after(&logged, Done::Range, Some(at(11, 0))),
        Done::Through(at(10, 3))
    );
    assert_eq!(
        done_after(&logged, Done::Range, Some(at(10, 0))),
        Done::Nothing
    );
    assert_eq!(done_after(&logged, Done::Range, None), Done::Range);
}

/// Review fix 6: an answer too large is asked again over half the blocks, down to one.
#[test]
fn a_range_halves_down_to_one_block() {
    let block = BlockNumber::new;
    assert_eq!(halved(block(100), block(599)), Some(block(349)));
    assert_eq!(halved(block(100), block(101)), Some(block(100)));
    assert_eq!(halved(block(100), block(100)), None);
}

/// Review fix 6: one pass at a time, and a refused one never ends the running one.
#[test]
fn one_pass_at_a_time() {
    let running = crate::guards::Running::take(&PASSING).unwrap();
    assert!(crate::guards::Running::take(&PASSING).is_none());
    assert!(
        crate::guards::Running::take(&PASSING).is_none(),
        "still running"
    );
    drop(running);
    assert!(crate::guards::Running::take(&PASSING).is_some());
}

/// The backup reads once the pool has settings (P11d: no registration says when a deposit
/// is on its way), and never before.
#[test]
fn the_backup_works_once_the_pool_has_settings() {
    on_fresh_memory(|| {
        crate::storage::init();
        assert!(!read_state(|state| has_work(state.store())));
        funded_pool(usdc(1_000));
        assert!(read_state(|state| has_work(state.store())));
    });
}

/// A chain whose every transfer to the canister is `logs`.
#[derive(Default)]
struct FakeBase {
    logs: Vec<serde_json::Value>,
    /// A block whose transfers are too large for one answer.
    overfull: Option<u64>,
    /// Receipts that fail, for now or for good.
    failing: Vec<(TxHash, PoolError)>,
    transfer_reads: Cell<usize>,
    receipts: std::cell::RefCell<Vec<TxHash>>,
    /// Receipts that carry more than a transfer of nothing to the canister.
    carrying: Vec<Receipt>,
    /// What the destination Portal answers of an intent's fill.
    fill: std::cell::RefCell<Option<Result<types::eco::Fill, String>>>,
}

impl crate::pool::arrival::FillReads for FakeBase {
    async fn fill(
        &self,
        _chain: ChainId,
        _intent: [u8; 32],
        _route_deadline: u64,
    ) -> Result<types::eco::Fill, String> {
        self.fill
            .borrow()
            .clone()
            .unwrap_or_else(|| Err("no fill read in this test".to_string()))
    }
}

impl BaseReads for FakeBase {
    async fn safe_block(&self) -> Result<BlockNumber, String> {
        // the lookback starts at block 100
        Ok(BlockNumber::new(100 + MAX_LOOKBACK_BLOCKS))
    }

    async fn transfers(&self, from: BlockNumber, to: BlockNumber) -> Result<Value, RpcError> {
        self.transfer_reads.set(self.transfer_reads.get() + 1);
        let within = |block: u64| from.get() <= block && block <= to.get();
        if self.overfull.is_some_and(within) {
            return Err(RpcError::AnswerTooLarge { cap: 1 });
        }
        Ok(Value::Array(
            self.logs
                .iter()
                .filter(|log| {
                    let block = log["blockNumber"].as_str().unwrap();
                    within(u64::from_str_radix(&block[2..], 16).unwrap())
                })
                .cloned()
                .collect(),
        ))
    }

    async fn receipt(&self, tx: TxHash) -> Result<Receipt, PoolError> {
        self.receipts.borrow_mut().push(tx);
        if let Some((_, error)) = self.failing.iter().find(|(failing, _)| *failing == tx) {
            return Err(error.clone());
        }
        if let Some(receipt) = self.carrying.iter().find(|receipt| receipt.tx == tx) {
            return Ok(receipt.clone());
        }
        // a transaction with no transfer to the canister: proves nothing, records nothing
        Ok(Receipt {
            tx,
            block: BlockNumber::new(1),
            success: true,
            logs: vec![],
        })
    }
}

fn chain() -> PoolChain {
    let address = |byte: u8| types::EvmAddress::new([byte; 20]);
    PoolChain {
        chain: ChainId::BASE,
        usdc: address(1),
        runner: address(2),
        swapic: address(3),
        transmitter: address(4),
        eco_portal: None,
        canister: address(5),
        cctp_sources: vec![],
    }
}

/// One transfer log of `amount` from a transaction told apart by `n`.
fn transfer(n: u32, block: u64, index: u64, amount: u64) -> serde_json::Value {
    let mut tx = [0_u8; 32];
    tx[..4].copy_from_slice(&n.to_be_bytes());
    serde_json::json!({
        "transactionHash": format!("0x{}", hex::encode(tx)),
        "blockNumber": format!("0x{block:x}"),
        "logIndex": format!("0x{index:x}"),
        "data": format!("0x{:064x}", amount),
    })
}

fn pass(base: &FakeBase) -> PassReport {
    block_on(backup(base, &chain(), || at(10))).unwrap()
}

/// Review (backup, denial of service): a block whose transfers are too large to read even
/// alone is passed over with an alert, and the cursor moves past it, never stalls; the
/// halving reads at most ten times; the next pass goes on after it.
#[test]
fn a_single_overfull_block_is_passed_over() {
    on_fresh_memory(|| {
        crate::storage::init();
        let base = FakeBase {
            logs: vec![transfer(1, 150, 0, 5_000_000)],
            overfull: Some(100),
            ..FakeBase::default()
        };
        let first = pass(&base);
        assert_eq!(first.cursor, Some((BlockNumber::new(100), None)));
        assert_eq!(first.receipts, 0);
        assert!(first.alerts[0].starts_with("ALERT: block 100"));
        assert!(base.transfer_reads.get() <= 10);
        // 101 and the 500 blocks from it
        let second = pass(&base);
        assert_eq!(second.receipts, 1);
        assert_eq!(second.cursor, Some((BlockNumber::new(600), None)));
    });
}

/// Review (backup, resource cap): a flood of distinct dust transactions reads no receipt,
/// and a pass considers at most its cap of transfers, the rest left to the next pass.
#[test]
fn a_flood_of_dust_reads_no_receipt() {
    on_fresh_memory(|| {
        crate::storage::init();
        // 1500 dust transfers, five a block from block 100
        let logs = (0..1_500_u32)
            .map(|n| transfer(n, 100 + u64::from(n / 5), u64::from(n % 5), DUST - 1))
            .collect();
        let base = FakeBase {
            logs,
            ..FakeBase::default()
        };
        let first = pass(&base);
        assert_eq!(first.receipts, 0);
        // done through the thousandth transfer, block 299's last
        assert_eq!(first.cursor, Some((BlockNumber::new(299), Some(4))));
        // from block 300 to the safe block
        let second = pass(&base);
        assert_eq!(second.receipts, 0);
        assert_eq!(second.cursor, Some((BlockNumber::new(700), None)));
        assert!(base.receipts.borrow().is_empty());
    });
}

/// Review (backup, resource cap, and the cursor rule): no pass reads more receipts than its
/// cap, whatever one block holds, and the read never passes a transfer worth reading that
/// it did not read: a block of thirty is read over two passes, from inside it. A receipt
/// that can never be read is passed over with an alert; one that failed for now holds the
/// cursor before it, and is read again.
#[test]
fn no_pass_reads_more_receipts_than_its_cap() {
    on_fresh_memory(|| {
        crate::storage::init();
        let worth = 5_000_000;
        let mut logs: Vec<_> = (0..30).map(|n| transfer(n, 120, n.into(), worth)).collect();
        logs.extend((30..45).map(|n| transfer(n, 130, n.into(), worth)));
        logs.extend((45..60).map(|n| transfer(n, 131, n.into(), worth)));
        logs.push(transfer(60, 140, 0, worth));
        logs.push(transfer(61, 145, 0, worth));
        let tx = |n: u32| {
            let mut tx = [0_u8; 32];
            tx[..4].copy_from_slice(&n.to_be_bytes());
            TxHash::new(tx)
        };
        let base = FakeBase {
            logs,
            failing: vec![
                (tx(60), PoolError::BadArrival("too large".to_string())),
                (
                    tx(61),
                    PoolError::Unavailable("the read failed".to_string()),
                ),
            ],
            ..FakeBase::default()
        };
        let cursor = |block: u64, log: Option<u64>| Some((BlockNumber::new(block), log));
        let passes: Vec<PassReport> = (0..5).map(|_| pass(&base)).collect();
        for one in &passes {
            assert!(one.receipts <= MAX_RECEIPTS_PER_PASS);
        }
        // block 120's first twenty, then its last ten and block 130's first ten
        assert_eq!(passes[0].cursor, cursor(120, Some(19)));
        assert_eq!(passes[1].cursor, cursor(130, Some(39)));
        assert_eq!(passes[2].cursor, cursor(131, Some(59)));
        // the receipt that never reads is passed over; the one that failed for now holds the
        // cursor before it, and is read again, never passed
        assert!(passes[3].alerts[0].contains("cannot be read"));
        assert_eq!(passes[3].cursor, cursor(140, Some(0)));
        assert_eq!(passes[4].receipts, 1);
        assert_eq!(passes[4].cursor, None);
        // every transaction worth reading was read before the cursor passed it
        let read: std::collections::BTreeSet<TxHash> =
            base.receipts.borrow().iter().copied().collect();
        assert!((0..61).all(|n| read.contains(&tx(n))));
    });
}

/// Review (backup, overlapping tick): a tick while a pass is out returns before reading
/// anything (were it to go on, it would read the clock, which only a canister has).
#[test]
fn a_tick_while_a_pass_is_out_does_nothing() {
    let running = crate::guards::Running::take(&PASSING).unwrap();
    block_on(run_backup());
    assert!(
        crate::guards::Running::take(&PASSING).is_none(),
        "the running pass still holds"
    );
    drop(running);
}

/// M-04: an Eco refund whose intent's fill could not be read is held like a receipt that
/// failed to read: the cursor stops before it and the next pass asks again, which credits
/// it once the fill reads. Before, the cursor moved past it and the refund was lost.
#[test]
fn an_eco_refund_whose_fill_was_not_read_holds_the_cursor() {
    use crate::pool::arrival::Log;
    use crate::storage::pool::tests::{eco_order, sign_payout_with, waiting_withdrawal};
    use crate::storage::pool::{payout, record_at};
    use types::pool::{PayoutStatus, PoolStep, Reclaim};
    on_fresh_memory(|| {
        crate::storage::init();
        let intent = [0x1e; 32];
        let id = waiting_withdrawal(
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
        );
        sign_payout_with(id, 0, 7, eco_order(1_800_000_000));
        record_at(
            PoolStep::PayoutPaid {
                id,
                tx_hash: TxHash::new([7; 32]),
                block: BlockNumber::new(30),
                intent: Some(intent),
            },
            at(4),
        )
        .unwrap();
        let amount = payout(id).unwrap().amount.unwrap();
        let portal = types::EvmAddress::new([6; 20]);
        let chain = PoolChain {
            eco_portal: Some(portal),
            ..chain()
        };
        // the Portal pays the canister back the intent's reward, at block 150
        let logged = transfer(
            1,
            150,
            3,
            u64::from_be_bytes(amount.to_be_bytes()[24..].try_into().unwrap()),
        );
        let tx = TxHash::new(parse_hash32(Some(&logged["transactionHash"])).unwrap());
        let topic = |address: types::EvmAddress| address.to_word();
        let refund = Receipt {
            tx,
            block: BlockNumber::new(150),
            success: true,
            logs: vec![
                Log {
                    index: 2,
                    address: portal,
                    topics: vec![
                        types::swapic::topics::intent_refunded(),
                        topic(chain.canister),
                    ],
                    data: intent.to_vec(),
                    block_time: None,
                },
                Log {
                    index: 3,
                    address: chain.usdc,
                    topics: vec![
                        types::swapic::topics::transfer(),
                        topic(portal),
                        topic(chain.canister),
                    ],
                    data: amount.to_be_bytes().to_vec(),
                    block_time: None,
                },
            ],
        };
        let base = FakeBase {
            logs: vec![logged],
            carrying: vec![refund],
            ..FakeBase::default()
        };
        // an hour past the route's deadline: its fill can be read
        let late = |secs: u64| Timestamp::from_secs(1_800_003_600 + secs).unwrap();
        let first = block_on(backup(&base, &chain, || late(0))).unwrap();
        assert_eq!(first.receipts, 1);
        assert!(
            first.cursor.is_none_or(
                |(block, log)| (block, log.unwrap_or(u64::MAX)) < (BlockNumber::new(150), 3)
            ),
            "the cursor stops before the refund: {:?}",
            first.cursor
        );
        assert_eq!(payout(id).unwrap().reclaim, None, "nothing credited yet");
        // the fill reads now: the next pass reads the refund again and credits it once
        *base.fill.borrow_mut() = Some(Ok(types::eco::Fill::Open));
        let second = block_on(backup(&base, &chain, || late(1))).unwrap();
        assert_eq!(second.receipts, 1, "the refund is read again");
        let refunded = payout(id).unwrap();
        assert_eq!(refunded.status, PayoutStatus::Unfilled);
        assert_eq!(refunded.reclaim, Some(Reclaim::Credited));
    });
}

/// L-15: the canister's dust floor is the services' ten cents (no pool route delivers less):
/// a stranger's five cents is never worth a receipt's read, and never an orphan for a human.
#[test]
fn a_transfer_under_ten_cents_is_dust() {
    on_fresh_memory(|| {
        crate::storage::init();
        let base = FakeBase {
            logs: vec![
                transfer(1, 150, 0, 50_000),
                transfer(2, 151, 0, 99_999),
                transfer(3, 152, 0, 100_000),
            ],
            ..FakeBase::default()
        };
        let report = pass(&base);
        assert_eq!(report.receipts, 1, "only the ten cents is read");
    });
}

/// The review (2026-10-24): the read of Base stops before a transaction whose Eco refund is
/// not proven yet, so it is read again and credited once proven. One whose fill was read too
/// early (no agreed block can be past its route's deadline yet) waits for its time quietly;
/// only one whose read failed is an ALERT.
#[test]
fn a_refund_read_too_early_holds_the_cursor_with_no_alert() {
    let found = |proof: Proof| {
        vec![crate::pool::arrival::Found {
            credit: CreditKey {
                chain: ChainId::BASE,
                tx: TxHash::new([0x41; 32]),
                log_index: 1,
            },
            amount: usdc(5),
            block: BlockNumber::new(40),
            at: None,
            proof,
            rail: types::pool::PoolRail::Eco,
        }]
    };
    assert_eq!(
        holds_back(&found(Proof::TooEarly { from: 1_800 })),
        (true, None)
    );
    let (held, why) = holds_back(&found(Proof::Unproven {
        why: "the read failed".to_string(),
    }));
    assert!(held);
    assert!(why.is_some_and(|why| why.contains("the read failed")));
    assert_eq!(
        holds_back(&found(Proof::Refund {
            intents: vec![[1; 32]]
        })),
        (false, None)
    );
}
