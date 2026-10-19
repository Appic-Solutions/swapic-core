use super::*;
use crate::storage::events::event_count;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{
    account, at, funded_pool, payer, settings, swap_terms, usdc, FEE,
};
use crate::storage::pool::{deposit, ledger};
use crate::task_manager::expiry_sweep::run_expiry_sweep;
use candid::{Nat, Principal};
use types::pool::DepositStatus;
use types::TokenAmount;

const NOW: u64 = 1_800_000_000_000_000_000;

fn now() -> Timestamp {
    at(NOW)
}

fn in_minutes(minutes: u64) -> Timestamp {
    at(NOW + minutes * 60_000_000_000)
}

fn nobody(_: &Address) -> bool {
    false
}

fn good() -> Registration {
    swap_terms(1, usdc(100), usdc(1), in_minutes(10))
}

fn try_register(terms: Registration) -> Result<(), PoolError> {
    let r#ref = terms.pool_ref().unwrap();
    register(r#ref, terms, now(), nobody)
}

#[test]
fn a_good_registration_reserves() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let terms = good();
        let r#ref = terms.pool_ref().unwrap();
        assert_eq!(register(r#ref, terms, now(), nobody), Ok(()));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
        assert_eq!(ledger().pockets.icp.reserved, usdc(99));
        assert_eq!(ledger().pockets.icp.free, usdc(901));
        // the same ref twice
        assert_eq!(try_register(good()), Err(PoolError::Known));
    });
}

#[test]
fn every_refusal_is_the_interfaces() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let count = event_count();
        // the quoter sent a ref for another premium than the terms carry
        let terms = good();
        let other_fee = Registration {
            premium: usdc(2),
            ..good()
        };
        assert_eq!(
            register(terms.pool_ref().unwrap(), other_fee, now(), nobody),
            Err(PoolError::BadRef)
        );
        // expired, or further away than the order's deadline and the rail allow
        let past = swap_terms(1, usdc(100), usdc(1), now());
        assert_eq!(try_register(past), Err(PoolError::Expired));
        let far = swap_terms(1, usdc(100), usdc(1), in_minutes(16));
        assert!(matches!(try_register(far), Err(PoolError::NotAllowed(_))));
        // a token the pool does not pay out
        let other = Principal::from_slice(&[9; 10]);
        let mut unlisted = good();
        unlisted.kind = DepositKind::Swap {
            recipient: account(1),
            token_out: other,
            min_out: 1,
        };
        assert_eq!(
            try_register(unlisted),
            Err(PoolError::TokenNotListed(other))
        );
        // a sanctioned payer, a sanctioned owner
        let terms = good();
        let r#ref = terms.pool_ref().unwrap();
        let payer_banned = |address: &Address| *address == Address::from(payer());
        assert!(matches!(
            register(r#ref, terms.clone(), now(), payer_banned),
            Err(PoolError::Sanctioned(_))
        ));
        let owner_text = account(1).owner.to_text();
        let owner_banned = |address: &Address| address.as_str() == owner_text;
        assert!(matches!(
            register(r#ref, terms, now(), owner_banned),
            Err(PoolError::Sanctioned(_))
        ));
        // above the per-swap cap, below the premium's floor
        assert_eq!(
            try_register(swap_terms(1, usdc(1_001), usdc(2), in_minutes(10))),
            Err(PoolError::AboveSwapCap {
                cap: Nat::from(1_000_000_000_u64)
            })
        );
        assert_eq!(
            try_register(swap_terms(
                1,
                usdc(2),
                TokenAmount::from(FEE - 1),
                in_minutes(10)
            )),
            Err(PoolError::PremiumTooLow {
                least: Nat::from(FEE)
            })
        );
        // nothing refused was written
        assert_eq!(event_count(), count);
    });
}

#[test]
fn the_open_limit_and_the_held_cap_refuse() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        for byte in 1..=3 {
            try_register(swap_terms(byte, usdc(10), usdc(1), in_minutes(10))).unwrap();
        }
        assert_eq!(
            try_register(swap_terms(4, usdc(10), usdc(1), in_minutes(10))),
            Err(PoolError::NotAllowed("too many open deposits".into()))
        );
        // another payer: 27 held of 1000, half may be, 474 is one too many
        let mut big = swap_terms(4, usdc(475), usdc(1), in_minutes(10));
        big.src_payer = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        assert_eq!(
            try_register(big.clone()),
            Err(PoolError::Closed("the ICP pocket is short".into()))
        );
        big.min_usdc = usdc(474);
        assert_eq!(try_register(big), Ok(()));
    });
}

#[test]
fn a_stake_holds_tvl_room_and_nothing_on_icp() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(9_000));
        let stake = |byte: u8, min_usdc| Registration {
            kind: DepositKind::Stake {
                owner: PoolAccount::Icp(account(byte)),
            },
            premium: TokenAmount::ZERO,
            rebalance_share: TokenAmount::from(500_u32),
            ..swap_terms(byte, min_usdc, TokenAmount::ZERO, in_minutes(10))
        };
        try_register(stake(5, usdc(600))).unwrap();
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
        assert_eq!(ledger().stake_room, usdc(600));
        // 9000 in, 600 on its way: 401 more is past the cap
        assert_eq!(
            try_register(stake(6, usdc(401))),
            Err(PoolError::AboveTvlCap {
                cap: Nat::from(10_000_000_000_u64),
                would_be: Nat::from(10_001_000_000_u64),
            })
        );
        assert_eq!(try_register(stake(6, usdc(400))), Ok(()));
    });
}

/// The sweep expires a registration past its expiry and grace, releases its hold, and leaves
/// one still in its grace.
#[test]
fn the_sweep_expires_a_registration_and_releases_its_hold() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let early = swap_terms(1, usdc(100), usdc(1), in_minutes(1));
        let late = swap_terms(2, usdc(50), usdc(1), in_minutes(10));
        let (early_ref, late_ref) = (early.pool_ref().unwrap(), late.pool_ref().unwrap());
        try_register(early).unwrap();
        try_register(late).unwrap();
        assert_eq!(ledger().pockets.icp.reserved, usdc(148));
        let grace = settings().deposit_grace;
        // at the end of the grace the arrival is still the user's
        let end_of_grace = in_minutes(1).checked_add(grace).unwrap();
        let swept = run_expiry_sweep(end_of_grace);
        assert_eq!(swept.pool_expired, 0);
        let swept = run_expiry_sweep(at(end_of_grace.as_nanos() + 1));
        assert_eq!(swept.pool_expired, 1);
        assert_eq!(deposit(&early_ref).unwrap().status, DepositStatus::Expired);
        assert_eq!(
            deposit(&late_ref).unwrap().status,
            DepositStatus::Registered
        );
        assert_eq!(ledger().pockets.icp.reserved, usdc(49));
        assert_eq!(ledger().pockets.icp.free, usdc(951));
        // bounded: a cap of one leaves the other for the next pass
        try_register(swap_terms(3, usdc(10), usdc(1), in_minutes(2))).unwrap();
        let expiry = expire_due(in_minutes(30), 1);
        assert_eq!((expiry.expired, expiry.more), (1, true));
        let expiry = expire_due(in_minutes(30), 1);
        assert_eq!((expiry.expired, expiry.more), (1, false));
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
    });
}
