use super::*;
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::{CctpMinFee, EvmAddress, GasAmount, Wei};

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

fn chain(move_fee: u64, finish_fee: u64) -> EvmChain {
    EvmChain {
        swapic: EvmAddress::new([1; 20]),
        runner: EvmAddress::new([2; 20]),
        cctp_receiver: EvmAddress::new([3; 20]),
        usdc: EvmAddress::new([4; 20]),
        token_messenger: EvmAddress::new([5; 20]),
        message_transmitter: EvmAddress::new([6; 20]),
        gas_limit: PerAction::all(GasAmount::from(500_000_u64)),
        gas_budget: PerAction::all(Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction {
            pool_move: Some(TokenAmount::from(move_fee)),
            pool_move_finish: Some(TokenAmount::from(finish_fee)),
            ..PerAction::all(TokenAmount::ZERO)
        },
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
    }
}

fn config() -> Config {
    Config {
        evm_chains: ChainTable(BTreeMap::from([
            (ChainId::BASE, chain(20_000, 30_000)),
            (ChainId::ETHEREUM, chain(400_000, 500_000)),
        ])),
        cctp_min_fees: ChainTable(BTreeMap::from([(
            ChainId::ETHEREUM,
            CctpMinFee::new(1_000),
        )])),
        ..Config::default()
    }
}

#[test]
fn standard_offers_the_least_fee_and_fast_the_cap() {
    let settings = RebalanceSettings::default();
    let config = config();
    // Base charges no least fee: nothing at standard finality
    assert_eq!(
        max_fee(&config, ChainId::BASE, usdc(1_000), false, &settings),
        TokenAmount::ZERO
    );
    // fast: 2 bps of $1,000
    assert_eq!(
        max_fee(&config, ChainId::BASE, usdc(1_000), true, &settings),
        TokenAmount::from(200_000_u64)
    );
    // Ethereum's least fee, 1 bp (1,000 thousandths), is the floor even at standard
    assert_eq!(
        max_fee(&config, ChainId::ETHEREUM, usdc(1_000), false, &settings),
        TokenAmount::from(100_000_u64)
    );
}

#[test]
fn the_own_route_costs_its_fee_and_both_network_fees() {
    let settings = RebalanceSettings::default();
    let config = config();
    // Base to ICP: the move on Base, its finish on Ethereum
    assert_eq!(
        own_cost(
            &config,
            ChainId::BASE,
            MoveDirection::BaseToIcp,
            usdc(1_000),
            false,
            &settings
        ),
        TokenAmount::from(20_000_u64 + 500_000)
    );
    // ICP to Base, urgent: Ethereum's fast fee, the move on Ethereum, its finish on Base
    assert_eq!(
        own_cost(
            &config,
            ChainId::BASE,
            MoveDirection::IcpToBase,
            usdc(1_000),
            true,
            &settings
        ),
        TokenAmount::from(200_000_u64 + 400_000 + 30_000)
    );
    assert!(fast(
        false,
        &RebalanceSettings {
            standard_when_idle: false,
            ..settings
        }
    ));
}
