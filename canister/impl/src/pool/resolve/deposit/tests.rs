use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, credit, funded_pool, payer, register, swap_terms, usdc};
use crate::storage::pool::{deposit, ledger, record_at};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::payout::PayoutSettings;
use types::pool::{PayoutAttempt, PayoutPath, MAX_PAYOUT_ATTEMPTS};
use types::{BlockNumber, ChainId, EvmAddress, TokenAmount};

fn base_usdc() -> EvmAddress {
    EvmAddress::new([0x83; 20])
}

/// A pool with 1,000 ckUSDC on ICP, the payout settings, and Base signing pool transfers.
fn pool() {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        },
        at(1),
    )
    .unwrap();
    let chain = EvmChain {
        swapic: EvmAddress::new([0x21; 20]),
        runner: EvmAddress::new([0x22; 20]),
        cctp_receiver: EvmAddress::new([0x23; 20]),
        usdc: base_usdc(),
        token_messenger: EvmAddress::new([0x25; 20]),
        message_transmitter: EvmAddress::new([0x26; 20]),
        gas_limit: PerAction::all(types::GasAmount::from(90_000_u64)),
        gas_budget: PerAction::all(types::Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::from(50_000_u64)),
    };
    crate::storage::config::test_set(types::Config {
        evm_chains: ChainTable(BTreeMap::from([(ChainId::BASE, chain)])),
        pool_chain: Some(ChainId::BASE),
        ..crate::storage::config::get()
    });
}

/// A swap of 100 USDC for `byte` arrived on Base; `started` its payout started at `at(5)`;
/// then held for a human after its tenth failure.
fn held(byte: u8, started: bool) -> PoolRef {
    let r#ref = register(swap_terms(byte, usdc(100), usdc(1), at(1_000)));
    record_at(
        PoolStep::Arrived {
            r#ref,
            credit: credit(u64::from(byte)),
            amount: usdc(100),
            block: BlockNumber::new(40),
        },
        at(3),
    )
    .unwrap();
    if started {
        record_at(
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: at(5),
                    memo: r#ref.into_bytes(),
                    path: PayoutPath::Ckusdc,
                    amount: None,
                    token: None,
                },
            },
            at(5),
        )
        .unwrap();
    }
    for n in 1..=MAX_PAYOUT_ATTEMPTS {
        record_at(
            PoolStep::AttemptFailed {
                r#ref,
                why: format!("try {n}"),
            },
            at(6),
        )
        .unwrap();
    }
    r#ref
}

fn open() -> Result<(), PoolError> {
    Ok(())
}

fn closed() -> Result<(), PoolError> {
    Err(PoolError::Closed("the books are stale".to_string()))
}

fn status(r#ref: PoolRef) -> DepositStatus {
    deposit(&r#ref).unwrap().status
}

/// The halt first, then the controller, then a deposit that is held for a human; a deposit
/// a drive holds now is not resolved.
#[test]
fn only_a_controller_resolves_and_only_a_held_deposit() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let r#ref = held(1, true);
        let resolve_now = |halted, controller, r#ref| {
            resolve(
                halted,
                controller,
                r#ref,
                WireResolution::Retry,
                at(7),
                |_: &Address| false,
                open,
            )
        };
        assert_eq!(resolve_now(true, true, r#ref), Err(PoolError::Halted));
        assert!(matches!(
            resolve_now(false, false, r#ref),
            Err(PoolError::NotAllowed(_))
        ));
        assert_eq!(
            resolve_now(false, true, PoolRef::new([9; 32])),
            Err(PoolError::Unknown)
        );
        let arrived = register(swap_terms(2, usdc(100), usdc(1), at(1_000)));
        record_at(
            PoolStep::Arrived {
                r#ref: arrived,
                credit: credit(2),
                amount: usdc(100),
                block: BlockNumber::new(40),
            },
            at(3),
        )
        .unwrap();
        assert!(matches!(
            resolve_now(false, true, arrived),
            Err(PoolError::NotAllowed(why)) if why.contains("not held")
        ));
        let busy = crate::pool::payout::Busy::take(r#ref.into_bytes()).unwrap();
        assert!(matches!(
            resolve_now(false, true, r#ref),
            Err(PoolError::Unavailable(_))
        ));
        drop(busy);
        assert!(matches!(status(r#ref), DepositStatus::PayoutFailed(_)));
    });
}

/// `Retry` (the review, 2026-10-24): a started payout pays again with its own dedup pair,
/// and only while the ledger still deduplicates it, so a transfer that landed is answered
/// `Duplicate` and recorded paid once; past that, the controller reads the ledger. One that
/// never started (CR-08) is queued again whenever.
#[test]
fn a_started_deposit_is_retried_only_while_the_ledger_deduplicates_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let retry = |r#ref, now| {
            resolve(
                false,
                true,
                r#ref,
                WireResolution::Retry,
                now,
                |_: &Address| false,
                open,
            )
        };
        let inside = held(1, true);
        assert_eq!(retry(inside, at(7)), Ok(None));
        assert_eq!(status(inside), DepositStatus::Paying);
        let attempt = deposit(&inside).unwrap().payout.attempt.unwrap();
        assert_eq!(attempt.created_at, at(5));
        let late = held(2, true);
        let past = at(5).checked_add(DEDUP_WINDOW).unwrap();
        assert!(matches!(
            retry(late, past),
            Err(PoolError::NotAllowed(why)) if why.contains("read the ledger")
        ));
        assert!(matches!(status(late), DepositStatus::PayoutFailed(_)));
        let never = held(3, false);
        assert_eq!(retry(never, past), Ok(None));
        assert_eq!(status(never), DepositStatus::Arrived);
        assert!(ledger().queue.0.iter().any(|(queued, _)| *queued == never));
    });
}

/// `Refund`: a started payout is never refunded on Base (its transfer may have landed, its
/// answer lost). For one that never started, every refusal comes before anything is
/// recorded (a sanctioned payer, a closed pool); then what arrived is an orphan held for its
/// payer, and the transfer to sign is all of it less the least network fee, to the payer,
/// on Base's USDC.
#[test]
fn a_refunded_deposit_is_an_orphan_and_its_transfer_is_checked_first() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let refund = |r#ref, sanctioned: bool, open: fn() -> Result<(), PoolError>| {
            resolve(
                false,
                true,
                r#ref,
                WireResolution::Refund,
                at(7),
                move |_: &Address| sanctioned,
                open,
            )
        };
        let started = held(1, true);
        assert!(matches!(
            refund(started, false, open),
            Err(PoolError::NotAllowed(why)) if why.contains("never refunded")
        ));
        let r#ref = held(2, false);
        assert!(matches!(
            refund(r#ref, true, open),
            Err(PoolError::Sanctioned(_))
        ));
        assert!(matches!(
            refund(r#ref, false, closed),
            Err(PoolError::Closed(_))
        ));
        assert!(matches!(status(r#ref), DepositStatus::PayoutFailed(_)));
        let call = refund(r#ref, false, open)
            .unwrap()
            .expect("a transfer to sign");
        assert_eq!(call.key, credit(2));
        assert_eq!(call.to, payer());
        assert_eq!(call.usdc, base_usdc());
        assert_eq!(
            call.sent,
            usdc(100)
                .checked_sub(TokenAmount::from(50_000_u64))
                .unwrap()
        );
        assert_eq!(status(r#ref), DepositStatus::Refunded);
        let orphans = crate::pool::orphans::held(at(7));
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].payer, Some(payer().to_string()));
        assert!(matches!(
            refund(r#ref, false, open),
            Err(PoolError::NotAllowed(_))
        ));
    });
}
