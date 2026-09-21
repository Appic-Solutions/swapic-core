use super::*;
use crate::storage::on_fresh_memory;

fn address() -> EvmAddress {
    "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
        .parse()
        .expect("the fixture address parses")
}

/// A canister that has not derived yet holds nothing, and what it derives it keeps. The
/// zero address is a real address here, not the empty cell.
#[test]
fn an_underived_canister_holds_nothing_and_a_derived_one_keeps_its_address() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(), None);
        set(address());
        assert_eq!(get(), Some(address()));
        set(EvmAddress::ZERO);
        assert_eq!(
            get(),
            Some(EvmAddress::ZERO),
            "the zero address is an address, not an empty cell"
        );
    });
}

/// The stored form is the twenty bytes and nothing else, so the cell never moves.
#[test]
fn the_cached_address_round_trips_through_its_bytes() {
    let cached = CachedAddress(Some(address()));
    assert_eq!(cached.to_bytes().len(), 20);
    assert_eq!(CachedAddress::from_bytes(cached.to_bytes()), cached);
    let empty = CachedAddress::default();
    assert_eq!(empty.to_bytes().len(), 0);
    assert_eq!(CachedAddress::from_bytes(empty.to_bytes()), empty);
}
