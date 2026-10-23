use super::*;
use crate::ic::{Account, CallError};
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, ckusdc, funded_pool, usdc};
use crate::storage::pool::{ledger, record_at, shares_of};
use candid::Nat;
use std::cell::{Cell, RefCell};
use types::pool::PoolAccount;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake ledger never waits"),
    }
}

fn canister() -> Principal {
    Principal::from_slice(&[0xc1; 10])
}

/// The ledger's blocks as the fake answers them, and the reads made.
#[derive(Default)]
struct Blocks {
    blocks: RefCell<std::collections::BTreeMap<u64, LedgerTransfer>>,
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

/// An ICP stake of 10 ckUSDC by `account(byte)` started at `at(1)` whose pull never
/// answered.
fn lost_stake(byte: u8) -> PoolOpId {
    let id = PoolOpId::new([byte; 32]);
    record_at(
        PoolStep::StakeStarted {
            id,
            owner: PoolAccount::Icp(account(byte)),
            from: account(byte),
            amount: usdc(10),
            created_at: at(1),
        },
        at(1),
    )
    .unwrap();
    id
}

/// The pull of stake `id` by `account(byte)` as the ledger logs it.
fn its_pull(byte: u8, id: PoolOpId) -> LedgerTransfer {
    LedgerTransfer {
        from: Account::of(account(byte).owner, None),
        to: Account::of(canister(), None),
        amount: Nat::from(usdc(10)),
        memo: Some(id.into_bytes().to_vec()),
        created_at_time: Some(at(1).as_nanos()),
        spender: Some(Account::of(canister(), None)),
    }
}

/// Past the ledger's deduplication window after `at(1)`.
fn past_window() -> Timestamp {
    at(1).checked_add(DEDUP_WINDOW).unwrap()
}

fn resolve_with(
    blocks: &Blocks,
    halted: bool,
    controller: bool,
    id: PoolOpId,
    resolution: WireResolution,
    now: Timestamp,
) -> Result<(), PoolError> {
    block_on(resolve(
        || halted,
        controller,
        canister(),
        id,
        resolution,
        || now,
        blocks,
    ))
}

/// The halt first, then the controller, then a stake that is pending, and only once the
/// canister stops asking its pull again; a stake a drive holds now is not resolved.
#[test]
fn only_a_controller_resolves_a_stake_past_the_window() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let id = lost_stake(7);
        let blocks = Blocks::default();
        let resolve_at = |halted, controller, id, now| {
            resolve_with(
                &blocks,
                halted,
                controller,
                id,
                WireResolution::NotPulled,
                now,
            )
        };
        assert_eq!(
            resolve_at(true, true, id, past_window()),
            Err(PoolError::Halted)
        );
        assert!(matches!(
            resolve_at(false, false, id, past_window()),
            Err(PoolError::NotAllowed(_))
        ));
        assert_eq!(
            resolve_at(false, true, PoolOpId::new([9; 32]), past_window()),
            Err(PoolError::Unknown)
        );
        let early = at(1)
            .checked_add(DEDUP_WINDOW - std::time::Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            resolve_at(false, true, id, early),
            Err(PoolError::NotAllowed(why)) if why.contains("window")
        ));
        let held = crate::pool::payout::Busy::take(id.into_bytes()).unwrap();
        assert!(matches!(
            resolve_at(false, true, id, past_window()),
            Err(PoolError::Unavailable(_))
        ));
        drop(held);
        assert!(ledger().pending.contains_key(&id));
    });
}

/// `Pulled` (the review, 2026-10-24): its block read on the ckUSDC ledger first, and it must
/// be the stake's own pull; then its shares are minted at the price now for its owner, its
/// ckUSDC free on ICP, once. `NotPulled`: dropped, its room freed, nothing minted.
#[test]
fn a_lost_stake_is_minted_on_its_own_pull_or_dropped_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let pulled = lost_stake(7);
        let blocks = Blocks::default();
        blocks.blocks.borrow_mut().insert(42, its_pull(7, pulled));
        let shares = ledger().mint_now(usdc(10)).unwrap();
        let free = ledger().pockets.icp.free;
        assert_eq!(
            resolve_with(
                &blocks,
                false,
                true,
                pulled,
                WireResolution::Pulled { block: 42 },
                past_window()
            ),
            Ok(())
        );
        assert_eq!(*blocks.reads.borrow(), vec![(ckusdc(), 42)]);
        assert_eq!(shares_of(&PoolAccount::Icp(account(7))), shares);
        assert_eq!(
            ledger().pockets.icp.free,
            free.checked_add(usdc(10)).unwrap()
        );
        assert_eq!(
            resolve_with(
                &blocks,
                false,
                true,
                pulled,
                WireResolution::NotPulled,
                past_window()
            ),
            Err(PoolError::Unknown)
        );
        let dropped = lost_stake(8);
        let (room, total) = (ledger().stake_room, ledger().total_shares);
        assert_eq!(
            resolve_with(
                &blocks,
                false,
                true,
                dropped,
                WireResolution::NotPulled,
                past_window()
            ),
            Ok(())
        );
        assert_eq!(ledger().stake_room, room.checked_sub(usdc(10)).unwrap());
        assert_eq!(ledger().total_shares, total);
        assert_eq!(shares_of(&PoolAccount::Icp(account(8))), Shares::ZERO);
    });
}

/// A block that is not the stake's own pull mints nothing: none at all, a ledger that cannot
/// be read now (try later), another transfer in any field, or a block named once already
/// (refused before any read).
#[test]
fn a_lost_stake_is_never_minted_on_a_block_that_is_not_its_pull() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let id = lost_stake(7);
        let blocks = Blocks::default();
        let claim = |block| {
            resolve_with(
                &blocks,
                false,
                true,
                id,
                WireResolution::Pulled { block },
                past_window(),
            )
        };
        assert!(matches!(claim(42), Err(PoolError::NotAllowed(why)) if why.contains("no block")));
        blocks.unreadable.set(true);
        assert!(matches!(claim(42), Err(PoolError::Unavailable(why)) if why.contains("later")));
        blocks.unreadable.set(false);
        let pull = its_pull(7, id);
        let stranger = Account::of(Principal::from_slice(&[0xee; 10]), None);
        let others = [
            LedgerTransfer {
                from: stranger.clone(),
                ..pull.clone()
            },
            LedgerTransfer {
                to: stranger.clone(),
                ..pull.clone()
            },
            LedgerTransfer {
                spender: None,
                ..pull.clone()
            },
            LedgerTransfer {
                amount: Nat::from(usdc(9)),
                ..pull.clone()
            },
            LedgerTransfer {
                memo: Some(vec![0; 32]),
                ..pull.clone()
            },
            LedgerTransfer {
                created_at_time: Some(at(2).as_nanos()),
                ..pull.clone()
            },
        ];
        for (block, other) in (50_u64..).zip(others) {
            blocks.blocks.borrow_mut().insert(block, other);
            assert!(
                matches!(claim(block), Err(PoolError::NotAllowed(_))),
                "block {block}"
            );
        }
        assert!(ledger().pending.contains_key(&id));
        // its own pull, then the same block named for another stake: refused unread
        blocks.blocks.borrow_mut().insert(42, pull);
        assert_eq!(claim(42), Ok(()));
        let another = lost_stake(8);
        let reads = blocks.reads.borrow().len();
        assert!(matches!(
            resolve_with(
                &blocks,
                false,
                true,
                another,
                WireResolution::Pulled { block: 42 },
                past_window()
            ),
            Err(PoolError::NotAllowed(why)) if why.contains("already")
        ));
        assert_eq!(blocks.reads.borrow().len(), reads);
    });
}
