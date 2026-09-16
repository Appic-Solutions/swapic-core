use super::*;

#[test]
fn named_chains_carry_their_eip155_ids() {
    assert_eq!(ChainId::ETHEREUM.get(), 1);
    assert_eq!(ChainId::BSC.get(), 56);
    assert_eq!(ChainId::POLYGON.get(), 137);
    assert_eq!(ChainId::BASE.get(), 8453);
    assert_eq!(ChainId::ARBITRUM.get(), 42161);
}

#[test]
fn display_prints_the_id() {
    assert_eq!(ChainId::BASE.to_string(), "8453");
    assert_eq!(ChainId::new(10).to_string(), "10");
}

/// A stable map orders keys by their bytes, which must be the numeric order.
#[test]
fn stored_chain_ids_sort_numerically() {
    use ic_stable_structures::Storable;

    let ids = [
        ChainId::ETHEREUM,
        ChainId::new(255),
        ChainId::new(256),
        ChainId::ARBITRUM,
    ];
    let bytes: Vec<_> = ids.iter().map(|id| id.to_bytes().into_owned()).collect();
    assert!(bytes.windows(2).all(|pair| pair[0] < pair[1]));
    for id in ids {
        assert_eq!(ChainId::from_bytes(id.to_bytes()), id);
    }
}
