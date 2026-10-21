use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, funded_pool, register, swap_terms, usdc};
use types::pool::PoolRail;

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

#[test]
fn every_transaction_is_read_once() {
    let logs = serde_json::json!([
        {"transactionHash": format!("0x{}", "11".repeat(32))},
        {"transactionHash": format!("0x{}", "22".repeat(32))},
        {"transactionHash": format!("0x{}", "11".repeat(32))},
        {"transactionHash": format!("0x{}", "33".repeat(32)), "removed": true},
    ]);
    assert_eq!(
        transactions(&logs),
        vec![TxHash::new([0x11; 32]), TxHash::new([0x22; 32])]
    );
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
