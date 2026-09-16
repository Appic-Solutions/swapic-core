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
