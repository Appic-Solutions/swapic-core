use crate::client::settlement::{get_chain_data, push_chain_data};
use crate::settlement_suite::init::{quoter, setup, upgrade, watcher};
use candid::{Nat, Principal};
use settlement_api::types::chain_data::{ChainData, ChainDataEntry};
use settlement_api::types::errors::{GuardError, PushChainDataError, Role};

const BASE: u64 = 8453;
const ARBITRUM: u64 = 42161;

fn stranger() -> Principal {
    Principal::from_slice(&[9; 29])
}

fn reading(block: u64) -> ChainData {
    ChainData {
        block,
        base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
        priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
    }
}

fn now_ns(pic: &pocket_ic::PocketIc) -> u64 {
    pic.get_time().as_nanos_since_unix_epoch()
}

/// The watcher's push is what the cache holds, and the instant on it is the canister's own
/// clock: the pushed record carries no instant at all, so nothing a caller sends can date
/// its data forward and keep stale gas prices alive.
#[test]
fn the_watcher_push_is_readable_and_stamped_with_canister_time() {
    let (pic, canister, _admin) = setup();
    assert_eq!(get_chain_data(&pic, canister, stranger(), BASE), None);

    assert_eq!(
        push_chain_data(&pic, canister, watcher(), BASE, &reading(19_000_000)),
        Ok(())
    );
    let entry = get_chain_data(&pic, canister, stranger(), BASE).expect("the push is readable");
    assert_eq!(entry.data, reading(19_000_000));
    assert_eq!(
        entry.pushed_at_ns,
        now_ns(&pic),
        "the stamp is the canister's clock"
    );
    assert_eq!(
        get_chain_data(&pic, canister, stranger(), ARBITRUM),
        None,
        "one push is one chain"
    );

    // a second push replaces the entry and restamps it
    pic.advance_time(std::time::Duration::from_secs(30));
    assert_eq!(
        push_chain_data(&pic, canister, watcher(), BASE, &reading(19_000_100)),
        Ok(())
    );
    let entry = get_chain_data(&pic, canister, stranger(), BASE).expect("the push is readable");
    assert_eq!(entry.data.block, 19_000_100);
    assert_eq!(entry.pushed_at_ns, now_ns(&pic));
}

/// Ambient chain data is the watcher's to push and nobody else's, and a refusal writes
/// nothing: the cache is empty afterwards.
#[test]
fn a_stranger_pushing_chain_data_is_refused_and_writes_nothing() {
    let (pic, canister, admin) = setup();
    for sender in [stranger(), quoter(), admin] {
        assert_eq!(
            push_chain_data(&pic, canister, sender, BASE, &reading(19_000_000)),
            Err(PushChainDataError::Guard(GuardError::CallerNotRole(
                Role::Watcher
            ))),
            "{sender} is not the watcher"
        );
    }
    assert_eq!(
        get_chain_data(&pic, canister, stranger(), BASE),
        None,
        "a refused push leaves no data behind"
    );
}

/// A fee no 256-bit price holds is refused at the edge, named, and stores nothing.
#[test]
fn a_fee_above_256_bits_is_refused_and_stores_nothing() {
    let (pic, canister, _admin) = setup();
    let too_large = Nat::parse(
        b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
    )
    .unwrap();
    let answer = push_chain_data(
        &pic,
        canister,
        watcher(),
        BASE,
        &ChainData {
            base_fee_wei_per_gas: too_large,
            ..reading(19_000_000)
        },
    );
    assert!(
        matches!(answer, Err(PushChainDataError::InvalidData(_))),
        "{answer:?}"
    );
    assert_eq!(get_chain_data(&pic, canister, stranger(), BASE), None);
}

/// The cache survives an upgrade, like every other piece of state.
#[test]
fn the_chain_data_cache_survives_an_upgrade() {
    let (pic, canister, admin) = setup();
    push_chain_data(&pic, canister, watcher(), BASE, &reading(19_000_000)).unwrap();
    let before: Option<ChainDataEntry> = get_chain_data(&pic, canister, stranger(), BASE);
    upgrade(&pic, canister, admin).expect("the upgrade goes through");
    assert_eq!(get_chain_data(&pic, canister, stranger(), BASE), before);
}
