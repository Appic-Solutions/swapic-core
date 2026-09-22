//! The rails: how a swap's funds cross from the source vault to the destination vault.
//!
//! A rail is a pure decision: given a swap as the fold holds it, its quote, the config and
//! what the inboxes hold, it answers the next move, and the engine is what sends, reads and
//! appends. Every transaction a rail asks for goes out through `tx::create_and_send`, so
//! rules A4 to A6 hold for a rail by construction. No rail is the default: the rail the
//! accepted quote names is the one that runs.

pub mod cctp;
pub mod eco;

#[cfg(test)]
pub(crate) mod tests;

pub use cctp::{MessageField, MessageMismatch};

use crate::deposits::VaultError;
use thiserror::Error;
use types::cctp::MessageError;
use types::events::TxPurpose;
use types::quote::QuoteAddressError;
use types::rail::RailTokenError;
use types::{
    Attestation, ChainId, Config, EcoIntent, EvmAddress, GasAmount, Quote, QuoteHash, Rail, Swap,
    TokenAmount, TxHash, UnixSeconds, Wei,
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
    /// Read the receipt of `tx_hash` on `chain_id`, the rail's mint, for what it delivered
    /// to the destination vault: the engine appends `PaidInStable` for that amount, which
    /// is what the chain says arrived and never what the burn promised.
    ReadMint { chain_id: ChainId, tx_hash: TxHash },
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
    #[error("the {rail} rail is not available on this deploy")]
    RailDisabled { rail: Rail },
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("the fee of {amount} does not fit an amount")]
    FeeOverflow { amount: TokenAmount },
    #[error(transparent)]
    RailToken(#[from] RailTokenError),
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
    #[error("the attestation's message is not a burn message: {0}")]
    UnreadableMessage(#[from] MessageError),
    #[error(transparent)]
    Message(#[from] MessageMismatch),
    #[error("the mint confirmed with no transaction hash recorded for it")]
    NoMintHash,
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
    match cctp::Cctp::of(rail) {
        Some(cctp) => Box::new(cctp),
        None => Box::new(eco::Eco),
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

impl From<RailError> for settlement_api::types::entry::RailError {
    fn from(error: RailError) -> Self {
        match error {
            RailError::NoDomain { chain_id } => Self::NoDomain {
                chain_id: chain_id.get(),
            },
            RailError::NoUsdc { chain_id } => Self::NoUsdc {
                chain_id: chain_id.get(),
            },
            RailError::NoTokenMessenger => Self::NoTokenMessenger,
            RailError::NoMessageTransmitter => Self::NoMessageTransmitter,
            RailError::NoEcoPortal => Self::NoEcoPortal,
            RailError::RailDisabled { rail } => Self::RailDisabled {
                rail: rail.to_string(),
            },
            RailError::Vault(error) => Self::Vault(error.into()),
            RailError::FeeOverflow { amount } => Self::FeeOverflow {
                amount: amount.into(),
            },
            RailError::RailToken(error) => Self::RailToken(error.into()),
            RailError::QuoteAddress(error) => Self::QuoteAddress(error.into()),
            RailError::UnreadableMessage(error) => Self::UnreadableMessage(error.into()),
            RailError::Message(mismatch) => Self::Message(mismatch.into()),
            RailError::NoMintHash => Self::NoMintHash,
        }
    }
}

impl From<MessageField> for settlement_api::types::entry::MessageField {
    fn from(field: MessageField) -> Self {
        match field {
            MessageField::SourceDomain => Self::SourceDomain,
            MessageField::DestinationDomain => Self::DestinationDomain,
            MessageField::Sender => Self::Sender,
            MessageField::Recipient => Self::Recipient,
            MessageField::DestinationCaller => Self::DestinationCaller,
            MessageField::BurnToken => Self::BurnToken,
            MessageField::MintRecipient => Self::MintRecipient,
            MessageField::Amount => Self::Amount,
            MessageField::MessageSender => Self::MessageSender,
            MessageField::MaxFee => Self::MaxFee,
        }
    }
}

impl From<MessageMismatch> for settlement_api::types::entry::MessageMismatch {
    fn from(mismatch: MessageMismatch) -> Self {
        match mismatch {
            MessageMismatch::Version { found } => Self::Version { found },
            MessageMismatch::Domain {
                field,
                expected,
                found,
            } => Self::Domain {
                field: field.into(),
                expected,
                found,
            },
            MessageMismatch::Word {
                field,
                expected,
                found,
            } => Self::Word {
                field: field.into(),
                expected,
                found,
            },
            MessageMismatch::Threshold { expected, found } => Self::Threshold { expected, found },
            MessageMismatch::Amount {
                field,
                expected,
                found,
            } => Self::Amount {
                field: field.into(),
                expected: expected.into(),
                found: found.into(),
            },
            MessageMismatch::FeeAboveMaxFee { fee, max_fee } => Self::FeeAboveMaxFee {
                fee: fee.into(),
                max_fee: max_fee.into(),
            },
            MessageMismatch::HookData { len } => Self::HookData {
                len: u64::try_from(len).expect("BUG: usize is at most 64 bits on every target"),
            },
        }
    }
}
