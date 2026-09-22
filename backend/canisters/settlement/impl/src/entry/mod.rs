//! The entry doors: where a user's funds become a swap, and where a gasless user's funds
//! are pulled into the vault so they can.
//!
//! `claim_swap` is the only creator of swaps and is money-first: every refusal that needs
//! no chain is made before an outcall is bought, a marker is committed before that outcall
//! (rule A8), and `FundsReceived` is appended only for a deposit the chain holds at depth,
//! with nothing stored when it does not. `start_gasless_pull` is pre-money: it sends the
//! vault's `pullWithPermit` through the one send path, and the deposit that transaction
//! makes is what a later claim verifies, so a pull creates no swap.

#[cfg(test)]
mod tests;

use crate::deposits::{self, DepositError, VaultError, VerifiedDeposit};
use crate::guards::{require_not_halted, require_quoter, require_quoter_or_watcher};
use crate::state::{pending_quotes, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::sanctions::is_sanctioned;
use crate::storage::{config, inflight, outbox};
use crate::tx::{self, TxError};
use settlement_api::types::entry::PullPermit;
use settlement_api::types::errors::GuardError;
use std::time::Duration;
use thiserror::Error;
use types::abi::vault_pull_with_permit;
use types::events::TxPurpose;
use types::evm::EvmAddressError;
use types::quote::QuoteError;
use types::{
    Address, EventType, EvmAddress, GasAmount, GasMode, InFlight, InFlightKind, Quote, QuoteHash,
    Timestamp, TokenAmount, TokenId, TxHash, UnixSeconds, Wei,
};

/// The gas a `pullWithPermit` needs: the permit's signature check and storage write, the
/// transfer, and the vault's own bookkeeping, with room over the measured cost.
const PULL_GAS_LIMIT: GasAmount = GasAmount::new(150_000);

/// Why `claim_swap` created no swap. Nothing was written.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ClaimError {
    #[error("refused before anything ran: {0:?}")]
    Guard(GuardError),
    #[error("the quote is not one this canister holds: {0}")]
    InvalidQuote(#[from] QuoteError),
    #[error("swap {0} already has funds")]
    SwapExists(QuoteHash),
    #[error(
        "the quote expired at {expires_at} and could be claimed until {claim_until}; it is {now}"
    )]
    QuoteExpired {
        expires_at: UnixSeconds,
        claim_until: UnixSeconds,
        now: UnixSeconds,
    },
    #[error("the quote's {party} is sanctioned")]
    Sanctioned { party: &'static str },
    #[error("a claim or a pull for this quote is already out since {}", .0.since)]
    InFlight(InFlight),
    #[error("the quote's source token {token} is not an EVM address: {reason}")]
    SourceTokenNotAnAddress {
        token: TokenId,
        reason: EvmAddressError,
    },
    #[error(transparent)]
    Deposit(#[from] DepositError),
    #[error("the vault holds a deposit of {deposited} for the quote, which is for {quoted}")]
    TokenMismatch {
        quoted: EvmAddress,
        deposited: EvmAddress,
    },
    #[error("the vault holds a deposit of {deposited} for the quote, which is for {quoted}")]
    AmountMismatch {
        quoted: TokenAmount,
        deposited: TokenAmount,
    },
    #[error(transparent)]
    Append(#[from] AppendError),
}

/// Why `start_gasless_pull` sent nothing.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PullError {
    #[error("refused before anything ran: {0:?}")]
    Guard(GuardError),
    #[error("quote {0} is not pending: never registered, or evicted")]
    UnknownQuote(QuoteHash),
    #[error("the quote's user pays their own gas")]
    NotGasless,
    #[error(
        "the quote expired at {expires_at} and could be paid until {claim_until}; it is {now}"
    )]
    QuoteExpired {
        expires_at: UnixSeconds,
        claim_until: UnixSeconds,
        now: UnixSeconds,
    },
    #[error("the permit's {field} is not the quote's")]
    PermitMismatch { field: &'static str },
    #[error("the {party} is sanctioned")]
    Sanctioned { party: &'static str },
    #[error("a claim or a pull for this quote is already out since {}", .0.since)]
    InFlight(InFlight),
    #[error("the pull {tx_hash} for this quote is still on its way to the chain")]
    AlreadyPulling { tx_hash: TxHash },
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Tx(#[from] TxError),
}

/// The last second a quote may still be claimed or paid: its expiry, and the window a
/// permit signed against it stays valid after that, which is also the window the pending
/// store keeps it for. `None` past the end of time, which holds every claim.
pub fn claim_deadline(quote: &Quote, permit_deadline: Duration) -> Option<UnixSeconds> {
    quote.expires_at.checked_add(permit_deadline)
}

/// The deadline the quote is past at `now`, if it is past one: what both doors refuse on.
pub fn missed_deadline(
    quote: &Quote,
    now: UnixSeconds,
    permit_deadline: Duration,
) -> Option<UnixSeconds> {
    claim_deadline(quote, permit_deadline).filter(|claim_until| now > *claim_until)
}

/// Refuses a quote nobody can pay any more, before an outcall is spent on it.
pub fn ensure_claimable(
    quote: &Quote,
    now: UnixSeconds,
    permit_deadline: Duration,
) -> Result<(), ClaimError> {
    match missed_deadline(quote, now, permit_deadline) {
        Some(claim_until) => Err(ClaimError::QuoteExpired {
            expires_at: quote.expires_at,
            claim_until,
            now,
        }),
        None => Ok(()),
    }
}

/// Which of the addresses the quote pays to is sanctioned, if either is: the destination
/// first, then the refund address.
pub fn sanctioned_party(quote: &Quote) -> Option<&'static str> {
    if is_sanctioned(&quote.dst_address) {
        return Some("dst_address");
    }
    if quote.refund_address.as_ref().is_some_and(is_sanctioned) {
        return Some("refund_address");
    }
    None
}

/// The quote's source token as the address the vault logs it under. A quote claimed on an
/// EVM chain names a token contract, and one that does not can match no deposit.
pub fn source_token(quote: &Quote) -> Result<EvmAddress, ClaimError> {
    quote
        .src_token
        .as_str()
        .parse()
        .map_err(|reason| ClaimError::SourceTokenNotAnAddress {
            token: quote.src_token.clone(),
            reason,
        })
}

/// The deposit has to be the quote's: its token, and exactly its amount. Another token
/// cannot ride the quote's rail, and another amount is neither the swap the user was quoted
/// nor one this canister can price, so both stay in the vault for an operator.
pub fn ensure_deposit_matches(
    quote: &Quote,
    token: EvmAddress,
    deposit: &VerifiedDeposit,
) -> Result<(), ClaimError> {
    if deposit.token != token {
        return Err(ClaimError::TokenMismatch {
            quoted: token,
            deposited: deposit.token,
        });
    }
    if deposit.amount != quote.amount_in {
        return Err(ClaimError::AmountMismatch {
            quoted: quote.amount_in,
            deposited: deposit.amount,
        });
    }
    Ok(())
}

/// The line that creates the swap, carrying the chain's truth: the token as the vault
/// logged it, the amount the vault measured as received, and the transaction the deposit
/// is in.
pub fn funds_received(quote: &Quote, deposit: &VerifiedDeposit) -> EventType {
    EventType::FundsReceived {
        quote_hash: quote.hash().expect("BUG: a validated quote has a preimage"),
        quote_bytes: quote
            .canonical_bytes()
            .expect("BUG: a validated quote has a preimage"),
        chain_id: quote.src_chain,
        token: deposit
            .token
            .to_string()
            .parse()
            .expect("BUG: an EIP-55 address is 42 bytes"),
        amount: deposit.amount,
        tx_ref: format!("0x{}", hex::encode(deposit.tx_ref.as_ref())),
    }
}

/// An address as the sanctions set reads it.
fn party(address: EvmAddress) -> Address {
    address
        .to_string()
        .parse()
        .expect("BUG: an EIP-55 address is 42 bytes")
}

/// Claims the deposit a user made for `quote` and creates its swap.
///
/// Money-first, in this order: the halt switch and the caller, the quote itself, the swap
/// not existing yet, the quote still claimable, nobody it pays to sanctioned, its token an
/// address; then the marker (A8), then the one outcall; then the deposit is held to the
/// quote and the payer to the sanctions set, and `FundsReceived` is appended, which the fold
/// checks again (A2). A refusal anywhere stores nothing.
pub async fn claim_swap(quote: Quote) -> Result<QuoteHash, ClaimError> {
    require_not_halted().map_err(ClaimError::Guard)?;
    require_quoter_or_watcher().map_err(ClaimError::Guard)?;
    quote.validate()?;
    let quote_hash = quote.hash().expect("BUG: a validated quote has a preimage");
    if read_state(|state| state.store().swap(&quote_hash).is_some()) {
        return Err(ClaimError::SwapExists(quote_hash));
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    ensure_claimable(&quote, now.as_secs(), config::get().permit_deadline)?;
    if let Some(party) = sanctioned_party(&quote) {
        return Err(ClaimError::Sanctioned { party });
    }
    let token = source_token(&quote)?;
    inflight::take(quote_hash, InFlightKind::Claim, now).map_err(ClaimError::InFlight)?;
    let verified = deposits::verify_evm_deposit(quote.src_chain, quote_hash).await;
    // the message chain ends here whatever the read said: the marker was for the outcall
    inflight::release(quote_hash);
    let deposit = verified?;
    ensure_deposit_matches(&quote, token, &deposit)?;
    if is_sanctioned(&party(deposit.from)) {
        return Err(ClaimError::Sanctioned { party: "from" });
    }
    append_event(funds_received(&quote, &deposit))?;
    Ok(quote_hash)
}

/// A gasless quote is the only kind the vault pulls for.
pub fn ensure_gasless(quote: &Quote) -> Result<(), PullError> {
    if quote.gas_mode != GasMode::Gasless {
        return Err(PullError::NotGasless);
    }
    Ok(())
}

/// The permit has to be the quote's own: the vault pulls the quote's token and amount, so
/// a permit for anything else either fails on the chain or takes the wrong funds.
pub fn ensure_permit_matches(
    quote: &Quote,
    token: EvmAddress,
    permit: &PullPermit,
) -> Result<(), PullError> {
    if permit.token != token {
        return Err(PullError::PermitMismatch { field: "token" });
    }
    if permit.amount != quote.amount_in {
        return Err(PullError::PermitMismatch { field: "amount" });
    }
    Ok(())
}

/// Pulls a gasless user's funds into the vault with the permit they signed, and answers the
/// hash of the transaction that does it.
///
/// Pre-money: the quote must be pending and gasless, still payable, the permit its own, and
/// nobody it involves sanctioned; then the marker (A8), then the one send path, which
/// allocates, signs, records `PullSigned` and queues. The deposit the transaction makes is
/// what `claim_swap` then verifies, so nothing here creates a swap.
pub async fn start_gasless_pull(
    quote_hash: QuoteHash,
    permit: PullPermit,
) -> Result<TxHash, PullError> {
    require_not_halted().map_err(PullError::Guard)?;
    require_quoter().map_err(PullError::Guard)?;
    let quote =
        pending_quotes::get_pending(&quote_hash).ok_or(PullError::UnknownQuote(quote_hash))?;
    ensure_gasless(&quote)?;
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    if let Some(claim_until) = missed_deadline(&quote, now.as_secs(), config.permit_deadline) {
        return Err(PullError::QuoteExpired {
            expires_at: quote.expires_at,
            claim_until,
            now: now.as_secs(),
        });
    }
    let token = source_token(&quote).map_err(|_| PullError::PermitMismatch { field: "token" })?;
    ensure_permit_matches(&quote, token, &permit)?;
    if is_sanctioned(&party(permit.owner)) {
        return Err(PullError::Sanctioned { party: "owner" });
    }
    if let Some(party) = sanctioned_party(&quote) {
        return Err(PullError::Sanctioned { party });
    }
    let vault = deposits::vault_of(&config, quote.src_chain)?;
    // a second pull while the first is still on its way would only revert on the vault,
    // which marks a quote per payer, and pay gas for it
    if let Some(out) = outbox::find(TxPurpose::GaslessPull(quote_hash)) {
        return Err(PullError::AlreadyPulling {
            tx_hash: out.tx_hash(),
        });
    }
    inflight::take(quote_hash, InFlightKind::Pull, now).map_err(PullError::InFlight)?;
    let PullPermit {
        token,
        owner,
        amount,
        deadline,
        signature,
    } = permit;
    let sent = tx::create_and_send(
        TxPurpose::GaslessPull(quote_hash),
        quote.src_chain,
        vault,
        Wei::ZERO,
        vault_pull_with_permit(quote_hash, token, owner, amount, deadline, &signature),
        PULL_GAS_LIMIT,
    )
    .await;
    inflight::release(quote_hash);
    Ok(sent?)
}

impl From<ClaimError> for settlement_api::types::entry::ClaimError {
    fn from(error: ClaimError) -> Self {
        match error {
            ClaimError::Guard(error) => Self::Guard(error),
            ClaimError::InvalidQuote(error) => Self::InvalidQuote(error.into()),
            ClaimError::SwapExists(quote_hash) => Self::SwapExists(quote_hash.into_bytes()),
            ClaimError::QuoteExpired {
                expires_at,
                claim_until,
                now,
            } => Self::QuoteExpired {
                expires_at_s: expires_at.get(),
                claim_until_s: claim_until.get(),
                now_s: now.get(),
            },
            ClaimError::Sanctioned { party } => Self::Sanctioned {
                party: party.to_string(),
            },
            ClaimError::InFlight(marker) => Self::InFlight {
                since_ns: marker.since.as_nanos(),
            },
            ClaimError::SourceTokenNotAnAddress { token, reason } => {
                Self::SourceTokenNotAnAddress {
                    token: token.to_string(),
                    reason: reason.into(),
                }
            }
            ClaimError::Deposit(error) => Self::Deposit(error.into()),
            ClaimError::TokenMismatch { quoted, deposited } => Self::TokenMismatch {
                quoted: quoted.to_string(),
                deposited: deposited.to_string(),
            },
            ClaimError::AmountMismatch { quoted, deposited } => Self::AmountMismatch {
                quoted: quoted.into(),
                deposited: deposited.into(),
            },
            ClaimError::Append(error) => Self::Append(error.into()),
        }
    }
}

impl From<PullError> for settlement_api::types::entry::PullError {
    fn from(error: PullError) -> Self {
        match error {
            PullError::Guard(error) => Self::Guard(error),
            PullError::UnknownQuote(quote_hash) => Self::UnknownQuote(quote_hash.into_bytes()),
            PullError::NotGasless => Self::NotGasless,
            PullError::QuoteExpired {
                expires_at,
                claim_until,
                now,
            } => Self::QuoteExpired {
                expires_at_s: expires_at.get(),
                claim_until_s: claim_until.get(),
                now_s: now.get(),
            },
            PullError::PermitMismatch { field } => Self::PermitMismatch {
                field: field.to_string(),
            },
            PullError::Sanctioned { party } => Self::Sanctioned {
                party: party.to_string(),
            },
            PullError::InFlight(marker) => Self::InFlight {
                since_ns: marker.since.as_nanos(),
            },
            PullError::AlreadyPulling { tx_hash } => Self::AlreadyPulling {
                tx_hash: tx_hash.into_bytes(),
            },
            PullError::Vault(error) => Self::Vault(error.into()),
            PullError::Tx(error) => Self::Tx(error.into()),
        }
    }
}

impl From<VaultError> for settlement_api::types::entry::VaultError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::NoVault { chain_id } => Self::NoVault {
                chain_id: chain_id.get(),
            },
            VaultError::NotAnAddress { chain_id, reason } => Self::NotAnAddress {
                chain_id: chain_id.get(),
                reason: reason.into(),
            },
        }
    }
}

impl From<DepositError> for settlement_api::types::entry::DepositError {
    fn from(error: DepositError) -> Self {
        match error {
            DepositError::Vault(error) => Self::Vault(error.into()),
            DepositError::StaleChainData { chain_id } => Self::StaleChainData {
                chain_id: chain_id.get(),
            },
            DepositError::Rpc(error) => Self::Rpc(error.into()),
            DepositError::UnreadableHead => Self::UnreadableHead,
            DepositError::UnreadableLogs => Self::UnreadableLogs,
            DepositError::NotFound { quote_hash } => Self::NotFound {
                quote_hash: quote_hash.into_bytes(),
            },
            DepositError::NotConfirmed {
                block,
                latest,
                depth,
            } => Self::NotConfirmed {
                block: block.get(),
                latest: latest.get(),
                depth: depth.get(),
            },
        }
    }
}
