use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, sign_payout, waiting_withdrawal};
use crate::storage::pool::{payout, record_at, shares_of};
use std::time::Duration;
use types::pool::payout::Undo;
use types::pool::PoolAccount;
use types::EvmAddress;

fn staker() -> EvmAddress {
    "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap()
}

/// A withdrawal signed, reverted and stopped for a human: its order ran after all, as `used`
/// read it.
fn stopped_withdrawal() -> u64 {
    let id = waiting_withdrawal(staker());
    sign_payout(id, 0, 9);
    record_at(PoolStep::PayoutReverted { id, block: None }, at(10)).unwrap();
    record_at(
        PoolStep::PayoutStopped {
            id,
            why: "ALERT: its order ran after its revert".to_string(),
        },
        at(11),
    )
    .unwrap();
    id
}

#[test]
fn only_a_controller_resolves_and_only_a_stopped_payout() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        let resolve_now = |halted, controller, id| {
            resolve(halted, controller, id, WireResolution::Refund, at(12))
        };
        assert_eq!(resolve_now(true, true, id), Err(PoolError::Halted));
        assert!(matches!(
            resolve_now(false, false, id),
            Err(PoolError::NotAllowed(_))
        ));
        assert_eq!(resolve_now(false, true, 99), Err(PoolError::Unknown));
        // waiting: not stopped
        assert!(matches!(
            resolve_now(false, true, id),
            Err(PoolError::NotAllowed(why)) if why.contains("not stopped")
        ));
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Waiting);
    });
}

/// A stopped withdrawal refunded: its shares minted back in the same call, once.
#[test]
fn a_stopped_withdrawal_refunded_is_reminted_in_the_same_call() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = stopped_withdrawal();
        let owner = PoolAccount::Evm(staker());
        let burned = match payout(id).unwrap().undo {
            Undo::Remint { shares, .. } => shares,
            Undo::Refund { .. } => unreachable!(),
        };
        let before = shares_of(&owner);
        resolve(false, true, id, WireResolution::Refund, at(12)).unwrap();
        assert_eq!(
            payout(id).unwrap().status,
            PayoutStatus::Undone(types::pool::payout::REFUNDED.to_string())
        );
        assert_eq!(shares_of(&owner), before.checked_add(burned).unwrap());
        assert!(resolve(false, true, id, WireResolution::Refund, at(13)).is_err());
        assert!(crate::storage::events::verify_replay());
    });
}

/// A stopped withdrawal whose order ran after all, marked paid: paid, once.
#[test]
fn a_stopped_payout_marked_paid_is_paid_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = stopped_withdrawal();
        resolve(false, true, id, WireResolution::MarkPaid, at(12)).unwrap();
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Paid);
        let ledger = crate::storage::pool::ledger();
        assert_eq!(ledger.pockets.base.reserved, types::TokenAmount::ZERO);
        assert_eq!(ledger.owed, types::TokenAmount::ZERO);
        assert!(resolve(false, true, id, WireResolution::MarkPaid, at(13)).is_err());
    });
}

/// A refund keeps its undo's dedup instant while a ledger still deduplicates it, so a refund
/// that went out and lost its answer is the same transfer; past that window, a new instant
/// (an old one would be refused as too old).
#[test]
fn a_refund_keeps_its_dedup_instant_within_the_ledgers_window() {
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(staker());
        let mut payout = payout(id).unwrap();
        let now = at(1_000);
        assert_eq!(refund_at(&payout, now), now);
        payout.undoing_at = Some(at(500));
        assert_eq!(refund_at(&payout, now), at(500));
        let late = at(500).checked_add(DEDUP_WINDOW).unwrap();
        assert_eq!(refund_at(&payout, late), late);
        let _ = Duration::ZERO;
    });
}

/// A ledger block names an account with its subaccount absent, all zero, or set; the ICP
/// ledger names it by its identifier. Absent and all zero are one account; a subaccount of
/// another length is no account at all.
#[test]
fn a_blocks_party_is_the_expected_account_written_either_way() {
    let owner = Principal::from_slice(&[7; 29]);
    let account =
        |subaccount: Option<Vec<u8>>| Party::Account(crate::ic::Account { owner, subaccount });
    let default = IcpAccount {
        owner,
        subaccount: None,
    };
    let zero = IcpAccount {
        owner,
        subaccount: Some([0; 32]),
    };
    let set = IcpAccount {
        owner,
        subaccount: Some([5; 32]),
    };
    for expected in [default, zero] {
        assert!(same_account(&account(None), expected));
        assert!(same_account(&account(Some(vec![0; 32])), expected));
        assert!(!same_account(&account(Some(vec![5; 32])), expected));
        assert!(!same_account(&account(Some(vec![0; 31])), expected));
        assert!(same_account(
            &Party::Identifier(expected.identifier()),
            expected
        ));
    }
    assert!(same_account(&account(Some(vec![5; 32])), set));
    assert!(!same_account(&account(None), set));
    assert!(!same_account(&account(Some(vec![5; 33])), set));
    assert!(!same_account(&Party::Identifier(default.identifier()), set));
    let stranger = Party::Account(crate::ic::Account {
        owner: Principal::from_slice(&[8; 29]),
        subaccount: None,
    });
    assert!(!same_account(&stranger, default));
}
