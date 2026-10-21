use super::*;
use crate::storage::events::append_event_at;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, created, funded_pool, usdc};
use crate::storage::pool::{ledger, record_at};
use types::pool::PayoutSettings;
use types::{BlockNumber, ChainId, EvmAddress, TxHash};

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("nothing here waits"),
    }
}

fn payer() -> EvmAddress {
    "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
        .parse()
        .unwrap()
}

fn key(log: u64) -> CreditKey {
    CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0x0f; 32]),
        log_index: log,
    }
}

/// A pool with an orphan of 3 USDC under `key(log)`.
fn orphaned(log: u64) {
    record_at(
        PoolStep::Unattributed {
            credit: key(log),
            amount: usdc(3),
            src_payer: Some(payer()),
            why: "no deposit is registered under the ref".to_string(),
            block: BlockNumber::new(40),
            r#ref: None,
        },
        at(2),
    )
    .unwrap();
}

#[test]
fn the_held_orphans_are_listed_with_their_payer() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        orphaned(1);
        let listed = held();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, key(1).to_bytes().to_vec());
        assert_eq!(listed[0].amount, nat(usdc(3)));
        assert_eq!(listed[0].payer, Some(payer().to_string()));
        assert_eq!(key_of(&listed[0].key), Ok(key(1)));
        assert!(key_of(&[0; 47]).is_err());
    });
}

/// Booked once: out of the pool's unattributed line, into the Base books', and never again.
#[test]
fn an_orphan_is_booked_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        orphaned(1);
        let before = ledger().unattributed;
        let book = || {
            block_on(resolve(
                true,
                key(1).to_bytes().to_vec(),
                OrphanAction::Book,
                at(3),
                |_| false,
            ))
        };
        assert_eq!(book(), Ok(()));
        assert_eq!(ledger().unattributed, before.checked_sub(usdc(3)).unwrap());
        let books = read_state(|state| state.book(&ChainId::BASE));
        assert_eq!(books.unattributed, usdc(3));
        assert!(held().is_empty());
        assert!(matches!(book(), Err(PoolError::NotAllowed(_))));
        assert!(crate::storage::events::verify_replay());
    });
}

#[test]
fn a_stranger_and_a_halt_are_refused() {
    assert_eq!(checked(true, true), Err(PoolError::Halted));
    assert!(matches!(
        checked(false, false),
        Err(PoolError::NotAllowed(_))
    ));
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        orphaned(1);
        assert!(matches!(
            block_on(resolve(
                false,
                key(1).to_bytes().to_vec(),
                OrphanAction::Book,
                at(3),
                |_| false
            )),
            Err(PoolError::NotAllowed(_))
        ));
        assert_eq!(held().len(), 1);
    });
}

/// Refunded once: its transfer's nonce is held for it alone, its signed record takes it off
/// the unattributed line (the least network fee kept on the Base fee line), a second refund
/// is refused, and a revert holds it again.
#[test]
fn an_orphan_is_refunded_once_and_held_again_after_a_revert() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(1),
        )
        .unwrap();
        orphaned(1);
        let orphan = read_state(|state| state.store().pool_orphan(&key(1))).unwrap();
        let sent = refund_amount(&orphan, PayoutSettings::default().min_network_fee).unwrap();
        assert_eq!(
            sent,
            usdc(3).checked_sub(TokenAmount::from(50_000_u32)).unwrap()
        );
        let purpose = TxPurpose::PoolTransfer(orphan_key(&key(1)));
        let signed = |nonce: u64| PoolStep::OrphanRefundSigned {
            key: key(1),
            to: payer(),
            sent,
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(nonce),
            tx_hash: TxHash::new([0x1f; 32]),
            raw_tx: vec![0x02],
        };
        let before = ledger();
        append_event_at(created(purpose, 0), at(3)).unwrap();
        record_at(signed(0), at(3)).unwrap();
        let after = ledger();
        assert_eq!(
            after.unattributed,
            before.unattributed.checked_sub(usdc(3)).unwrap()
        );
        assert_eq!(after.lines().fees_base, TokenAmount::from(50_000_u32));
        assert!(held().is_empty());
        // once: no second refund is created for it
        assert!(append_event_at(created(purpose, 1), at(4)).is_err());
        // reverted at depth: held again, every line back
        record_at(
            PoolStep::OrphanRefundClosed {
                key: key(1),
                success: false,
            },
            at(5),
        )
        .unwrap();
        assert_eq!(held().len(), 1);
        assert_eq!(ledger().unattributed, before.unattributed);
        assert_eq!(ledger().lines().fees_base, TokenAmount::ZERO);
        // a fee that takes all of it: book it instead
        let small = Orphan {
            amount: TokenAmount::from(50_000_u32),
            ..orphan
        };
        assert_eq!(refund_amount(&small, TokenAmount::from(50_000_u32)), None);
        assert!(crate::storage::events::verify_replay());
    });
}
