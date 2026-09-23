//! The fixtures the two rails' tests share: a Base to Arbitrum USDC quote, a swap at any
//! point of its life, and a config with every rail knob set.
use super::Position;
use std::collections::BTreeMap;
use types::config::ChainTable;
use types::{
    Attestation, ChainId, Config, EcoIntent, EvmAddress, GasMode, Leg as SwapLeg, Outcome, Quote,
    Rail, Swap, SwapStatus, TokenAmount, UnixSeconds,
};

pub const VAULT_BASE: &str = "0x1111111111111111111111111111111111111111";
pub const VAULT_ARBITRUM: &str = "0x2222222222222222222222222222222222222222";
pub const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
pub const USDC_ARBITRUM: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
pub const MINE: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

pub fn quote() -> Quote {
    Quote {
        version: 1,
        src_chain: ChainId::BASE,
        src_token: USDC_BASE.parse().unwrap(),
        amount_in: TokenAmount::from(25_000_000_u32),
        dst_chain: ChainId::ARBITRUM,
        dst_token: USDC_ARBITRUM.parse().unwrap(),
        expected_out: TokenAmount::from(24_990_000_u32),
        min_out: TokenAmount::from(24_900_000_u32),
        dst_address: MINE.parse().unwrap(),
        refund_address: Some(MINE.parse().unwrap()),
        auto_refund: true,
        gas_mode: GasMode::Legacy,
        rail: Rail::CctpV2Fast,
        expires_at: UnixSeconds::new(1_800_000_000),
        nonce: 1,
    }
}

/// The swap of `quote()`, at the point its latest leg says.
pub fn fixture_swap(last_leg: Option<SwapLeg>, last_outcome: Option<Outcome>) -> Swap {
    let quote = quote();
    Swap {
        quote_bytes: quote.canonical_bytes().unwrap(),
        status: SwapStatus::Executing,
        last_attempt: None,
        open_attempt: None,
        src_chain: quote.src_chain,
        src_token: quote.src_token,
        amount_in: quote.amount_in,
        amount_paid: None,
        waiting_since: None,
        last_leg,
        last_outcome,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
    }
}

pub fn config() -> Config {
    Config {
        vault_addresses: BTreeMap::from([
            (ChainId::BASE, VAULT_BASE.parse().unwrap()),
            (ChainId::ARBITRUM, VAULT_ARBITRUM.parse().unwrap()),
        ]),
        cctp_domains: ChainTable(BTreeMap::from([
            (ChainId::BASE, types::CctpDomain::new(6)),
            (ChainId::ARBITRUM, types::CctpDomain::new(3)),
        ])),
        usdc_addresses: ChainTable(BTreeMap::from([
            (ChainId::BASE, USDC_BASE.parse().unwrap()),
            (ChainId::ARBITRUM, USDC_ARBITRUM.parse().unwrap()),
        ])),
        token_messenger: Some(
            "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
                .parse()
                .unwrap(),
        ),
        message_transmitter: Some(
            "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"
                .parse()
                .unwrap(),
        ),
        eco_portal: Some(
            "0xEC000064576f9C95a8623Bc0eff3db6d296ea6df"
                .parse()
                .unwrap(),
        ),
        // the rail is off on a deploy; the Eco tests turn it on for themselves, and one
        // of them pins what the rail does while it is off
        eco_enabled: types::config::EcoEnabled::ON,
        ..Config::default()
    }
}

pub fn at<'a>(
    quote: &'a Quote,
    swap: &'a Swap,
    config: &'a Config,
    attestation: Option<&'a Attestation>,
    intent: Option<&'a EcoIntent>,
) -> Position<'a> {
    Position {
        quote_hash: quote.hash().unwrap(),
        quote,
        swap,
        config,
        mine: MINE.parse::<EvmAddress>().unwrap(),
        attestation,
        intent,
        now: UnixSeconds::new(1_000),
    }
}
