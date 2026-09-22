//! The rails: how a swap's funds cross from the source vault to the destination vault.
//!
//! A rail is a pure decision: given a swap as the fold holds it, its quote, the config and
//! what the inboxes hold, it answers the next move, and the engine is what sends, reads and
//! appends. Every transaction a rail asks for goes out through `tx::create_and_send`, so
//! rules A4 to A6 hold for a rail by construction. No rail is the default: the rail the
//! accepted quote names is the one that runs.

pub mod cctp;
pub mod eco;

use crate::deposits::VaultError;
use thiserror::Error;
use types::events::TxPurpose;
use types::quote::QuoteAddressError;
use types::rail::RailTokenError;
use types::{
    Attestation, ChainId, Config, EcoIntent, EvmAddress, GasAmount, Quote, QuoteHash, Rail, Swap,
    TokenAmount, UnixSeconds, Wei,
};

/// One transaction a rail asks the engine to send, ready for `tx::create_and_send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RailTx {
    pub purpose: TxPurpose,
    pub chain_id: ChainId,
    pub to: EvmAddress,
    pub value: Wei,
    pub data: Vec<u8>,
    pub gas_limit: GasAmount,
}

/// What a rail is waiting on from outside before it can move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitingFor {
    /// Circle's attestation of the burn, which the watcher hands in.
    Attestation,
    /// Eco's quote response for the swap, which the watcher hands in.
    Intent,
    /// The intent's deadline, after which the reward can be reclaimed.
    Deadline,
}

/// The rail's next move for a swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RailStep {
    /// Send this transaction: the burn, the publish or the mint.
    Send(RailTx),
    /// Nothing to send yet.
    Wait(WaitingFor),
    /// Read the destination vault for the quote's deposit: the rail's funds arrive there
    /// without a transaction of this canister's. `expired` says the rail's deadline has
    /// passed, so a read that finds nothing ends the swap in a refund.
    CheckArrival { chain_id: ChainId, expired: bool },
    /// The stable landed on the destination with the rail's last transaction: the engine
    /// appends `PaidInStable` for `amount` on `chain_id`.
    Arrived {
        chain_id: ChainId,
        amount: TokenAmount,
    },
    /// Send this transaction to take the funds back into the source vault, for a refund to
    /// follow.
    Reclaim(RailTx),
    /// No leg leads from here: the engine freezes the swap with this reason.
    Stuck(&'static str),
}

/// Why a rail could not decide: a knob the deploy left unset, or an address not derived.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RailError {
    #[error("chain {chain_id} has no CCTP domain configured")]
    NoDomain { chain_id: ChainId },
    #[error("chain {chain_id} has no USDC address configured")]
    NoUsdc { chain_id: ChainId },
    #[error("no CCTP token messenger is configured")]
    NoTokenMessenger,
    #[error("no CCTP message transmitter is configured")]
    NoMessageTransmitter,
    #[error("no Eco portal is configured")]
    NoEcoPortal,
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("the fee of {amount} does not fit an amount")]
    FeeOverflow { amount: TokenAmount },
    #[error(transparent)]
    RailToken(#[from] RailTokenError),
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
}

/// What a rail decides on: the swap as the fold holds it, its quote, the deploy's config,
/// this canister's own address, what the inboxes hold for the swap, and the clock.
pub struct Leg<'a> {
    pub quote_hash: QuoteHash,
    pub quote: &'a Quote,
    pub swap: &'a Swap,
    pub config: &'a Config,
    /// This canister's EVM address: the only account allowed to deliver its mints.
    pub mine: EvmAddress,
    pub attestation: Option<&'a Attestation>,
    pub intent: Option<&'a EcoIntent>,
    pub now: UnixSeconds,
}

/// A rail this canister drives with its own transactions.
pub trait CallRail {
    fn rail(&self) -> Rail;

    /// The next move for a swap executing on this rail. Asked with no attempt open, when
    /// the funds have arrived and after each of the rail's own legs confirmed.
    fn step(&self, leg: &Leg) -> Result<RailStep, RailError>;

    /// The move for a swap being refunded whose first leg confirmed: whether and how the
    /// funds come back to the source vault, from where the user is refunded.
    fn reclaim(&self, leg: &Leg) -> Result<RailStep, RailError>;
}

/// A rail entered by sending the funds to an address the rail owns, which a later plan's
/// NEAR-style rails are. Declared now so the engine's shape admits them without a change.
pub trait DepositRail {
    fn rail(&self) -> Rail;

    /// Where the funds for `quote` are sent to enter the rail.
    fn deposit_address(&self, quote: &Quote) -> Result<types::Address, RailError>;
}

/// The rail a quote names, and never any other.
pub fn for_rail(rail: Rail) -> Box<dyn CallRail> {
    match rail {
        Rail::CctpV2Fast => Box::new(cctp::Cctp { fast: true }),
        Rail::CctpV2Standard => Box::new(cctp::Cctp { fast: false }),
        Rail::Eco => Box::new(eco::Eco),
    }
}

/// The USDC contract on `chain_id`, or the knob that is unset.
fn usdc_on(config: &Config, chain_id: ChainId) -> Result<EvmAddress, RailError> {
    config
        .usdc_addresses
        .get(chain_id)
        .ok_or(RailError::NoUsdc { chain_id })
}

/// Both of the quote's tokens pinned to the rail before any leg is built: the rails carry
/// the configured USDC and nothing else, so a swap naming another token on either side
/// (one the claim would have refused, or one whose config moved since) has the vault's
/// USDC spent for nothing, or a token paid out that the mint never delivered. Refused by
/// the field, and retried on the next tick rather than frozen, because a knob an operator
/// moves is what puts a claimed swap here.
fn ensure_rail_tokens(leg: &Leg) -> Result<(), RailError> {
    Ok(types::rail::ensure_rail_tokens(
        &leg.config.usdc_addresses,
        leg.quote,
    )?)
}

/// The fixtures the two rails' tests share: a Base to Arbitrum USDC quote, a swap at any
/// point of its life, and a config with every rail knob set.
#[cfg(test)]
pub(crate) mod tests {
    use super::Leg;
    use std::collections::BTreeMap;
    use types::config::ChainTable;
    use types::{
        Attestation, ChainId, Config, EcoIntent, EvmAddress, GasMode, Leg as SwapLeg, Outcome,
        Quote, Rail, Swap, SwapStatus, TokenAmount, UnixSeconds,
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
            ..Config::default()
        }
    }

    pub fn leg<'a>(
        quote: &'a Quote,
        swap: &'a Swap,
        config: &'a Config,
        attestation: Option<&'a Attestation>,
        intent: Option<&'a EcoIntent>,
    ) -> Leg<'a> {
        Leg {
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
}
