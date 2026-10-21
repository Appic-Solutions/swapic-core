use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, funded_pool, register, swap_terms, usdc};
use types::pool::{CreditKey, PoolRail};

#[test]
fn a_read_covers_at_most_five_hundred_blocks_from_its_cursor() {
    let block = BlockNumber::new;
    // no cursor: from the lookback
    assert_eq!(
        range(None, block(10_000)),
        Some((block(9_400), block(9_899)))
    );
    // a cursor behind the head: from the block after it
    assert_eq!(
        range(Some(block(9_950)), block(10_000)),
        Some((block(9_951), block(10_000)))
    );
    // a cursor older than the lookback: from the lookback
    assert_eq!(
        range(Some(block(1)), block(10_000)),
        Some((block(9_400), block(9_899)))
    );
    // nothing new
    assert_eq!(range(Some(block(10_000)), block(10_000)), None);
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

/// Review fix 6: a pass reads each transaction once, never one whose every transfer is
/// credited or dust, at most the cap of receipts in whole blocks (the first block always),
/// and says which block is left for the next pass.
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
    let tx = |byte: u8, block: u64| (TxHash::new([byte; 32]), BlockNumber::new(block));
    assert_eq!(
        plan_pass(&logged, ChainId::BASE, credited, 20),
        (vec![tx(1, 10), tx(4, 11), tx(5, 11), tx(6, 12)], None)
    );
    // a cap of two: block 10 whole, then block 11 does not fit and is left
    assert_eq!(
        plan_pass(&logged, ChainId::BASE, credited, 2),
        (vec![tx(1, 10)], Some(BlockNumber::new(11)))
    );
    // a cap of zero still reads the first block, so the cursor moves
    assert_eq!(
        plan_pass(&logged, ChainId::BASE, credited, 0),
        (vec![tx(1, 10)], Some(BlockNumber::new(11)))
    );
}

/// Review fix 6: an answer too large is asked again over half the blocks, down to one; the
/// cursor stops before the first block left, and stays where it is when that is the first.
#[test]
fn a_range_halves_and_the_cursor_stops_before_what_is_left() {
    let block = BlockNumber::new;
    assert_eq!(halved(block(100), block(599)), Some(block(349)));
    assert_eq!(halved(block(100), block(101)), Some(block(100)));
    assert_eq!(halved(block(100), block(100)), None);
    assert_eq!(cursor_after(block(100), block(599), None), Some(block(599)));
    assert_eq!(
        cursor_after(block(100), block(599), Some(block(250))),
        Some(block(249))
    );
    assert_eq!(cursor_after(block(100), block(599), Some(block(100))), None);
}

/// Review fix 6: one pass at a time, and a refused one never ends the running one.
#[test]
fn one_pass_at_a_time() {
    let running = Pass::take().unwrap();
    assert!(Pass::take().is_none());
    assert!(Pass::take().is_none(), "still running");
    drop(running);
    assert!(Pass::take().is_some());
}

/// The backup reads only while a CCTP or same-chain registration waits: an Eco one is the
/// watcher's, and a registration past its grace waits no more.
#[test]
fn the_backup_works_only_while_a_registration_it_can_prove_waits() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let now = at(500);
        assert!(!read_state(|state| has_work(state.store(), now)));
        let mut eco = swap_terms(1, usdc(10), usdc(1), at(1_000));
        eco.rail = PoolRail::Eco;
        register(eco);
        assert!(!read_state(|state| has_work(state.store(), now)));
        register(swap_terms(2, usdc(10), usdc(1), at(1_000)));
        assert!(read_state(|state| has_work(state.store(), now)));
        let grace = types::pool::PoolSettings::default()
            .deposit_grace
            .as_nanos() as u64;
        // read one margin past its grace (review fix 13), and no more after
        let margin =
            read_state(|state| pool_store::read_margin_in(state.store())).as_nanos() as u64;
        assert!(read_state(|state| has_work(
            state.store(),
            at(1_000 + grace + margin)
        )));
        assert!(!read_state(|state| has_work(
            state.store(),
            at(1_000 + grace + margin + 1)
        )));
    });
}
