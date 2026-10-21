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

/// Jeff's hold rule (2026-10-21): a good registration is recorded and holds nothing.
#[test]
fn a_good_registration_holds_nothing() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let terms = good();
        let r#ref = terms.pool_ref().unwrap();
        assert_eq!(register(r#ref, terms, now(), nobody), Ok(()));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
        assert_eq!(ledger().pockets.icp.free, usdc(1_000));
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

/// The open limits and the free check at this moment refuse; nothing held, so each new
/// registration finds the same free ckUSDC.
#[test]
fn the_open_limit_and_the_free_check_refuse() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(500));
        for byte in 1..=3 {
            try_register(swap_terms(byte, usdc(10), usdc(1), in_minutes(10))).unwrap();
        }
        assert_eq!(
            try_register(swap_terms(4, usdc(10), usdc(1), in_minutes(10))),
            Err(PoolError::NotAllowed("too many open deposits".into()))
        );
        // another payer: a payout of 501 is more than the 500 free, 500 fits, twice
        let other = |byte: u8, min_usdc| {
            let mut terms = swap_terms(byte, min_usdc, usdc(1), in_minutes(10));
            terms.src_payer = types::EvmAddress::new([byte; 20]);
            terms
        };
        assert_eq!(
            try_register(other(5, usdc(502))),
            Err(PoolError::Closed("the ICP pocket is short".into()))
        );
        assert_eq!(try_register(other(5, usdc(501))), Ok(()));
        assert_eq!(try_register(other(6, usdc(501))), Ok(()));
    });
}

/// A stake checks the TVL cap at registration and holds no room.
#[test]
fn a_stake_checks_the_tvl_cap_and_holds_nothing() {
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
        assert_eq!(
            try_register(stake(5, usdc(1_001))),
            Err(PoolError::AboveTvlCap {
                cap: Nat::from(10_000_000_000_u64),
                would_be: Nat::from(10_001_000_000_u64),
            })
        );
        try_register(stake(5, usdc(1_000))).unwrap();
        assert_eq!(ledger().stake_room, TokenAmount::ZERO);
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
        // nothing held: another finds the same room
        assert_eq!(try_register(stake(6, usdc(1_000))), Ok(()));
    });
}

/// The sweep expires a registration past its expiry and grace, and leaves one still in its
/// grace. Nothing was held, so nothing moves.
#[test]
fn the_sweep_expires_a_registration() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let early = swap_terms(1, usdc(100), usdc(1), in_minutes(1));
        let late = swap_terms(2, usdc(50), usdc(1), in_minutes(10));
        let (early_ref, late_ref) = (early.pool_ref().unwrap(), late.pool_ref().unwrap());
        try_register(early).unwrap();
        try_register(late).unwrap();
        let grace = settings().deposit_grace;
        // at the end of the grace the arrival is still the user's, and one read margin past
        // it an arrival on time may not have been read yet (review fix 13)
        let end_of_grace = in_minutes(1).checked_add(grace).unwrap();
        let margin = crate::storage::events::read_state(|state| {
            crate::storage::pool::read_margin_in(state.store())
        });
        let end_of_reading = end_of_grace.checked_add(margin).unwrap();
        let swept = run_expiry_sweep(end_of_grace);
        assert_eq!(swept.pool_expired, 0);
        let swept = run_expiry_sweep(end_of_reading);
        assert_eq!(swept.pool_expired, 0);
        let swept = run_expiry_sweep(at(end_of_reading.as_nanos() + 1));
        assert_eq!(swept.pool_expired, 1);
        assert_eq!(deposit(&early_ref).unwrap().status, DepositStatus::Expired);
        assert_eq!(
            deposit(&late_ref).unwrap().status,
            DepositStatus::Registered
        );
        assert_eq!(ledger().pockets.icp.free, usdc(1_000));
        // bounded: a cap of one leaves the other for the next pass
        try_register(swap_terms(3, usdc(10), usdc(1), in_minutes(2))).unwrap();
        let expiry = expire_due(in_minutes(30), 1);
        assert_eq!((expiry.expired, expiry.more), (1, true));
        let expiry = expire_due(in_minutes(30), 1);
        assert_eq!((expiry.expired, expiry.more), (1, false));
        assert!(ledger().open.is_empty());
    });
}

/// `pool_deposit_agreed` (P13a): after a commit that met `Known`, whether the registration
/// stored under the ref is this one. Reads, and writes nothing.
#[test]
fn a_known_registration_is_agreed_only_when_it_is_the_same() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let terms = good();
        let r#ref = terms.pool_ref().unwrap();
        let agreed_on = |terms: &Registration| read_state(|state| agreed(state, r#ref, terms));
        assert_eq!(agreed_on(&terms), Err(PoolError::Unknown));
        try_register(terms.clone()).unwrap();
        let count = event_count();
        assert_eq!(agreed_on(&terms), Ok(true));
        let later = Registration {
            expires_at: in_minutes(11),
            ..terms.clone()
        };
        assert_eq!(agreed_on(&later), Ok(false));
        let more = Registration {
            min_usdc: usdc(101),
            ..terms.clone()
        };
        assert_eq!(agreed_on(&more), Ok(false));
        let other_payer = Registration {
            src_payer: "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            ..terms.clone()
        };
        assert_eq!(agreed_on(&other_payer), Ok(false));
        let other_rail = Registration {
            rail: types::pool::PoolRail::Eco,
            ..terms
        };
        assert_eq!(agreed_on(&other_rail), Ok(false));
        assert_eq!(event_count(), count, "nothing was written");
    });
}
