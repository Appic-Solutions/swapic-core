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

/// What a wait for a block past a time reads: Ethereum's twelve seconds on a chain the
/// canister does not know, never less than a known chain's own.
#[test]
fn a_chain_names_the_seconds_its_block_takes_at_the_most() {
    assert_eq!(ChainId::ETHEREUM.block_seconds(), 12);
    assert_eq!(ChainId::BASE.block_seconds(), 2);
    assert_eq!(ChainId::ARBITRUM.block_seconds(), 1);
    assert_eq!(ChainId::new(5042).block_seconds(), 1);
    assert_eq!(ChainId::new(43114).block_seconds(), 2);
    assert_eq!(ChainId::BSC.block_seconds(), 1);
    assert_eq!(ChainId::POLYGON.block_seconds(), 2);
    assert_eq!(ChainId::new(999_999).block_seconds(), 12);
}

#[test]
fn the_chains_the_canister_signs_on_have_names_and_native_coins() {
    let named = [
        (1, "Ethereum", "ETH"),
        (10, "OP Mainnet", "ETH"),
        (130, "Unichain", "ETH"),
        (137, "Polygon", "POL"),
        (143, "Monad", "MON"),
        (480, "World Chain", "ETH"),
        (999, "HyperEVM", "HYPE"),
        (8453, "Base", "ETH"),
        (9745, "Plasma", "XPL"),
        (42161, "Arbitrum", "ETH"),
        (43114, "Avalanche", "AVAX"),
        (5042, "Arc", "USDC"),
        (56, "BNB Chain", "BNB"),
    ];
    for (id, name, symbol) in named {
        assert_eq!(ChainId::new(id).name(), Some(name), "{id}");
        assert_eq!(ChainId::new(id).native_symbol(), Some(symbol), "{id}");
    }
    // Robinhood Chain is not deployed: unnamed, so a message writes its id
    for id in [4663] {
        assert_eq!(ChainId::new(id).name(), None);
        assert_eq!(ChainId::new(id).native_symbol(), None);
    }
}
