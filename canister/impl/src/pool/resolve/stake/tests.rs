use super::*;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, funded_pool, usdc};
use crate::storage::pool::{ledger, record_at, shares_of};
use types::pool::PoolAccount;

/// An ICP stake of 10 ckUSDC started at `at(1)` whose pull never answered.
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

/// Past the ledger's deduplication window after `at(1)`.
fn past_window() -> Timestamp {
    at(1).checked_add(DEDUP_WINDOW).unwrap()
}

/// The halt first, then the controller, then a stake that is pending, and only once the
/// canister stops asking its pull again; a stake a drive holds now is not resolved.
#[test]
fn only_a_controller_resolves_a_stake_past_the_window() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let id = lost_stake(7);
        let resolve_at = |halted, controller, id, now| {
            resolve(halted, controller, id, WireResolution::NotPulled, now)
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

/// `Pulled`: its shares minted at the price now for its owner, its ckUSDC free on ICP, once.
/// `NotPulled`: dropped, its room freed, nothing minted.
#[test]
fn a_lost_stake_is_minted_or_dropped_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let pulled = lost_stake(7);
        let shares = ledger().mint_now(usdc(10)).unwrap();
        let free = ledger().pockets.icp.free;
        assert_eq!(
            resolve(
                false,
                true,
                pulled,
                WireResolution::Pulled { block: 42 },
                past_window()
            ),
            Ok(())
        );
        assert_eq!(shares_of(&PoolAccount::Icp(account(7))), shares);
        assert_eq!(ledger().pockets.icp.free, free.checked_add(usdc(10)).unwrap());
        assert_eq!(
            resolve(
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
            resolve(
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
