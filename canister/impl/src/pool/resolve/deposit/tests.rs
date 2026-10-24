use super::*;
use crate::ic::{Account, CallError, LedgerTransfer};
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, ckusdc, credit, funded_pool, payer, register, swap_terms, usdc,
};
use crate::storage::pool::{deposit, ledger, record_at};
use candid::Nat;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::payout::PayoutSettings;
use types::pool::{DepositKind, PayoutAttempt, PayoutPath, PrincipalBytes, MAX_PAYOUT_ATTEMPTS};
use types::{BlockNumber, ChainId, EvmAddress, TokenAmount};

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake ledger never waits"),
    }
}

fn base_usdc() -> EvmAddress {
    EvmAddress::new([0x83; 20])
}

fn canister() -> Principal {
    Principal::from_slice(&[0xc1; 10])
}

/// The ledgers' blocks as the fake answers them, and the reads made.
#[derive(Default)]
struct Blocks {
    blocks: RefCell<BTreeMap<u64, LedgerTransfer>>,
    unreadable: Cell<bool>,
    reads: RefCell<Vec<(Principal, u64)>>,
}

impl LedgerBlocks for Blocks {
    async fn transfer_at(
        &self,
        ledger: Principal,
        block: u64,
    ) -> Result<Option<LedgerTransfer>, CallError> {
        self.reads.borrow_mut().push((ledger, block));
        if self.unreadable.get() {
            return Err(CallError::Unknown("the ledger did not answer".to_string()));
        }
        Ok(self.blocks.borrow().get(&block).cloned())
    }
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

/// Where a held payout's transfer stood when it was held.
#[derive(Clone, Copy, PartialEq)]
enum Sent {
    /// Never started.
    Never,
    /// Started, no transfer asked.
    Unasked,
    /// Its transfer asked, its outcome unknown: it may have landed.
    Asked,
    /// Its transfer refused by the ledger for sure.
    Refused,
}

/// What a started payout of `account(byte)` asks: 98.99 ckUSDC.
fn sent_amount() -> TokenAmount {
    usdc(99).checked_sub(TokenAmount::from(10_000_u64)).unwrap()
}

/// A ckUSDC swap of 100 USDC for `account(byte)` arrived on Base, its payout where `sent`
/// says (started at `at(5)`), then held for a human after its tenth failure.
fn held_at(byte: u8, sent: Sent) -> PoolRef {
    held_paying(byte, sent, ckusdc(), PayoutPath::Ckusdc)
}

fn held_paying(byte: u8, sent: Sent, token_out: Principal, path: PayoutPath) -> PoolRef {
    let mut terms = swap_terms(byte, usdc(100), usdc(1), at(1_000));
    terms.kind = DepositKind::Swap {
        recipient: account(byte),
        token_out,
        min_out: 1,
    };
    let r#ref = register(terms);
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
    if sent != Sent::Never {
        record_at(
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: at(5),
                    memo: r#ref.into_bytes(),
                    path,
                    amount: Some(sent_amount()),
                    token: Some(PrincipalBytes(ckusdc())),
                },
            },
            at(5),
        )
        .unwrap();
    }
    if matches!(sent, Sent::Asked | Sent::Refused) {
        record_at(
            PoolStep::TransferAsked {
                r#ref,
                token: PrincipalBytes(token_out),
                amount: sent_amount(),
                spent: usdc(99),
            },
            at(5),
        )
        .unwrap();
    }
    if sent == Sent::Refused {
        record_at(PoolStep::TransferRefused { r#ref }, at(5)).unwrap();
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

/// The door, every guard and read given.
#[allow(clippy::too_many_arguments)]
fn resolve_with(
    blocks: &Blocks,
    halted: bool,
    controller: bool,
    r#ref: PoolRef,
    resolution: WireResolution,
    now: Timestamp,
    sanctioned: bool,
    open: fn() -> Result<(), PoolError>,
) -> Result<Option<Refund>, PoolError> {
    block_on(resolve(
        || halted,
        controller,
        canister(),
        r#ref,
        resolution,
        || now,
        move |_: &Address| sanctioned,
        open,
        blocks,
    ))
}

/// A controller's call at `now`, the pool open, no one sanctioned.
fn by_controller(
    blocks: &Blocks,
    r#ref: PoolRef,
    resolution: WireResolution,
    now: Timestamp,
) -> Result<Option<Refund>, PoolError> {
    resolve_with(blocks, false, true, r#ref, resolution, now, false, open)
}

/// The halt first, then the controller, then a deposit that is held for a human; a deposit
/// a drive holds now is not resolved.
#[test]
fn only_a_controller_resolves_and_only_a_held_deposit() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let r#ref = held_at(1, Sent::Unasked);
        let resolve_now = |halted, controller, r#ref| {
            resolve_with(
                &blocks,
                halted,
                controller,
                r#ref,
                WireResolution::Retry,
                at(7),
                false,
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
/// and only while the ledger still deduplicates it with an hour left, so a transfer that
/// landed is answered `Duplicate` and recorded paid once; past that, the controller is told
/// what to search the ledger for. One that never started (CR-08) is queued again whenever.
#[test]
fn a_started_deposit_is_retried_only_while_the_ledger_deduplicates_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let retry = |r#ref, now| by_controller(&blocks, r#ref, WireResolution::Retry, now);
        let inside = held_at(1, Sent::Asked);
        assert_eq!(retry(inside, at(7)), Ok(None));
        assert_eq!(status(inside), DepositStatus::Paying);
        let attempt = deposit(&inside).unwrap().payout.attempt.unwrap();
        assert_eq!(attempt.created_at, at(5));
        let late = held_at(2, Sent::Asked);
        let edge = at(5)
            .checked_add(DEDUP_WINDOW - std::time::Duration::from_secs(3_600))
            .unwrap();
        let memo = crate::rpc::hex0x(&late.into_bytes());
        assert!(matches!(
            retry(late, edge),
            Err(PoolError::NotAllowed(why))
                if why.contains(&memo) && why.contains(&at(5).as_nanos().to_string())
        ));
        assert!(matches!(status(late), DepositStatus::PayoutFailed(_)));
        assert_eq!(
            retry(late, Timestamp::from_nanos(edge.as_nanos() - 1)),
            Ok(None)
        );
        let never = held_at(3, Sent::Never);
        let past = at(5).checked_add(DEDUP_WINDOW).unwrap();
        assert_eq!(retry(never, past), Ok(None));
        assert_eq!(status(never), DepositStatus::Arrived);
        assert!(ledger().queue.0.iter().any(|(queued, _)| *queued == never));
    });
}

/// The review (2026-10-24): a started payout whose transfer may have landed on another
/// token's ledger is never retried with its old dedup pair (that ledger may take it as a new
/// transfer): `MarkPaid` it if it landed.
#[test]
fn a_started_deposit_that_may_have_paid_another_token_is_never_retried() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let token = Principal::from_slice(&[0x70; 10]);
        let r#ref = held_paying(
            1,
            Sent::Asked,
            token,
            PayoutPath::Icpswap {
                pool: Principal::from_slice(&[0x71; 10]),
            },
        );
        assert!(matches!(
            by_controller(&blocks, r#ref, WireResolution::Retry, at(7)),
            Err(PoolError::NotAllowed(why)) if why.contains("never retried")
        ));
        assert!(matches!(status(r#ref), DepositStatus::PayoutFailed(_)));
    });
}

/// The review (2026-10-24): a started payout whose transfer cannot land twice (never asked,
/// or refused by the ledger for sure) is retried whatever token it pays, at any time, asked
/// afresh: its dedup instant the retry's own.
#[test]
fn a_started_deposit_whose_transfer_cannot_land_twice_is_retried_afresh() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let past = at(5).checked_add(DEDUP_WINDOW).unwrap();
        let token = Principal::from_slice(&[0x70; 10]);
        let unasked = held_paying(
            1,
            Sent::Unasked,
            token,
            PayoutPath::Icpswap {
                pool: Principal::from_slice(&[0x71; 10]),
            },
        );
        let refused = held_at(2, Sent::Refused);
        for r#ref in [unasked, refused] {
            assert_eq!(
                by_controller(&blocks, r#ref, WireResolution::Retry, past),
                Ok(None)
            );
            assert_eq!(status(r#ref), DepositStatus::Paying);
            let attempt = deposit(&r#ref).unwrap().payout.attempt.unwrap();
            assert_eq!(attempt.created_at, past, "asked afresh");
        }
    });
}

/// `Refund` (the review, 2026-10-24): only when the canister knows nothing went out: never
/// started, started with no transfer asked, or its transfer refused for sure. A transfer that
/// may have landed is not refunded, and the controller is told what to search for. Every
/// refusal comes before anything is recorded (a sanctioned payer, a closed pool); then what
/// arrived is an orphan held for its payer, and the transfer to sign is all of it less the
/// least network fee, to the payer, on Base's USDC.
#[test]
fn a_deposit_is_refunded_only_when_nothing_went_out() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let refund = |r#ref, sanctioned: bool, open: fn() -> Result<(), PoolError>| {
            resolve_with(
                &blocks,
                false,
                true,
                r#ref,
                WireResolution::Refund,
                at(7),
                sanctioned,
                open,
            )
        };
        let asked = held_at(1, Sent::Asked);
        assert!(matches!(
            refund(asked, false, open),
            Err(PoolError::NotAllowed(why))
                if why.contains("may have") && why.contains(&crate::rpc::hex0x(&asked.into_bytes()))
        ));
        for (byte, sent) in [(2, Sent::Unasked), (3, Sent::Refused)] {
            let r#ref = held_at(byte, sent);
            assert!(refund(r#ref, false, open).unwrap().is_some());
            assert_eq!(status(r#ref), DepositStatus::Refunded);
        }
        let r#ref = held_at(4, Sent::Never);
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
        assert_eq!(call.key, credit(4));
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
        assert_eq!(orphans.len(), 3);
        assert!(matches!(
            refund(r#ref, false, open),
            Err(PoolError::NotAllowed(_))
        ));
    });
}

/// The transfer a started payout of `account(byte)` asked, as the ledger logs it.
fn its_transfer(byte: u8, r#ref: PoolRef) -> LedgerTransfer {
    LedgerTransfer {
        from: Account::of(canister(), None),
        to: Account::of(account(byte).owner, None),
        amount: Nat::from(sent_amount()),
        memo: Some(r#ref.into_bytes().to_vec()),
        created_at_time: Some(at(5).as_nanos()),
        spender: None,
    }
}

/// `MarkPaid { block }` (the review, 2026-10-24): the block read on the transfer's own ledger
/// first, and it must be exactly the transfer the payout asked; then the deposit is paid,
/// once, as asked. Any time: past the ledger's window too. A ledger not read now refuses, to
/// be asked later; a payout that never asked its transfer paid nothing.
#[test]
fn a_deposit_is_marked_paid_only_on_its_own_transfer() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let blocks = Blocks::default();
        let past = at(5).checked_add(DEDUP_WINDOW).unwrap();
        let mark =
            |r#ref, block| by_controller(&blocks, r#ref, WireResolution::MarkPaid { block }, past);
        let unasked = held_at(1, Sent::Unasked);
        assert!(matches!(mark(unasked, 42), Err(PoolError::NotAllowed(_))));
        let r#ref = held_at(2, Sent::Asked);
        assert!(matches!(
            mark(r#ref, 42),
            Err(PoolError::NotAllowed(why)) if why.contains("no block")
        ));
        blocks.unreadable.set(true);
        assert!(matches!(
            mark(r#ref, 42),
            Err(PoolError::Unavailable(why)) if why.contains("later")
        ));
        blocks.unreadable.set(false);
        let transfer = its_transfer(2, r#ref);
        let stranger = Account::of(Principal::from_slice(&[0xee; 10]), None);
        let others = [
            LedgerTransfer {
                from: stranger.clone(),
                ..transfer.clone()
            },
            LedgerTransfer {
                to: stranger.clone(),
                ..transfer.clone()
            },
            LedgerTransfer {
                spender: Some(stranger.clone()),
                ..transfer.clone()
            },
            LedgerTransfer {
                amount: Nat::from(usdc(99)),
                ..transfer.clone()
            },
            LedgerTransfer {
                memo: Some(vec![0; 32]),
                ..transfer.clone()
            },
            LedgerTransfer {
                created_at_time: Some(at(6).as_nanos()),
                ..transfer.clone()
            },
        ];
        for (block, other) in (50_u64..).zip(others) {
            blocks.blocks.borrow_mut().insert(block, other);
            assert!(
                matches!(mark(r#ref, block), Err(PoolError::NotAllowed(_))),
                "block {block}"
            );
        }
        assert!(matches!(status(r#ref), DepositStatus::PayoutFailed(_)));
        blocks.blocks.borrow_mut().insert(42, transfer);
        assert_eq!(mark(r#ref, 42), Ok(None));
        assert_eq!(blocks.reads.borrow().last(), Some(&(ckusdc(), 42)));
        let paid = deposit(&r#ref).unwrap();
        assert_eq!(paid.status, DepositStatus::Paid);
        assert_eq!(paid.paid.unwrap().block, 42);
        assert_eq!(paid.paid.unwrap().amount, sent_amount());
        assert!(matches!(mark(r#ref, 42), Err(PoolError::NotAllowed(_))));
    });
}
