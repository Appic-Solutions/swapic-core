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

use crate::deposits::{
    self, DepositError, DepositRead, VaultError, VerifiedDeposit, Wanted, WantedAmount,
};
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
use types::quote::QuoteError;
use types::quote::{QuoteAddressError, QuoteAddressField};
use types::rail::{ensure_rail_tokens, RailTokenError};
use types::{
    Address, Config, EventType, EvmAddress, GasAmount, GasMode, InFlight, InFlightKind, Quote,
    QuoteHash, Rail, Timestamp, TxHash, UnixSeconds, Wei,
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
    #[error("the {rail} rail is not available on this deploy")]
    RailUnavailable { rail: Rail },
    #[error(transparent)]
    RailToken(#[from] RailTokenError),
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
    #[error(transparent)]
    Deposit(#[from] DepositError),
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
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
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

/// The rail a quote names has to be one this deploy runs. The Eco rail is off until its
/// route is designed (see `rails::eco`), so a quote naming it is refused here, before an
/// outcall is bought, rather than becoming a swap whose funds the canister cannot move.
pub fn ensure_rail_is_enabled(config: &Config, quote: &Quote) -> Result<(), ClaimError> {
    if quote.rail == Rail::Eco && !config.eco_enabled.is_on() {
        return Err(ClaimError::RailUnavailable { rail: quote.rail });
    }
    Ok(())
}

/// A quote has to name a refund address a refund can be paid to. The fold does not hold
/// the payer (`FundsReceived` carries none and its layout is frozen), so the refund
/// address is the only way the user's funds come back: a swap without one can only be
/// frozen with the funds in the vault, so no swap is created for a quote that has none.
pub fn ensure_refundable(quote: &Quote) -> Result<(), ClaimError> {
    quote.evm_address(QuoteAddressField::RefundAddress)?;
    Ok(())
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

/// The quote's source token as the address the vault logs it under, once both of the
/// quote's tokens are pinned to its rail: the rails carry the USDC the deploy configured
/// and nothing else, so a quote naming any other token on either side is refused here,
/// by the field, before an outcall is bought. Otherwise the burn would spend the vault's
/// USDC against a deposit of whatever the quote named.
pub fn rail_source_token(config: &Config, quote: &Quote) -> Result<EvmAddress, ClaimError> {
    ensure_rail_tokens(&config.usdc_addresses, quote)?;
    Ok(quote.evm_address(QuoteAddressField::SrcToken)?)
}

/// The deposit the claim looks for: the quote's token, in exactly its amount. Another
/// token cannot ride the quote's rail, and another amount is neither the swap the user was
/// quoted nor one this canister can price, so neither is the deposit, whatever else the
/// vault holds under the hash, and it stays there for an operator.
pub fn wanted(quote: &Quote, token: EvmAddress) -> Wanted {
    Wanted {
        token,
        amount: WantedAmount::Exactly(quote.amount_in),
    }
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
/// not existing yet, the quote still claimable, nobody it pays to sanctioned, its tokens
/// the rail's; then the marker (A8), then the one read, which looks for the quote's token
/// in exactly its amount among the logs under the hash; then the payer is held to the
/// sanctions set, and `FundsReceived` is appended, which the fold checks again (A2). A
/// refusal anywhere stores nothing.
pub async fn claim_swap(quote: Quote) -> Result<QuoteHash, ClaimError> {
    require_not_halted().map_err(ClaimError::Guard)?;
    require_quoter_or_watcher().map_err(ClaimError::Guard)?;
    quote.validate()?;
    let quote_hash = quote.hash().expect("BUG: a validated quote has a preimage");
    if read_state(|state| state.store().swap(&quote_hash).is_some()) {
        return Err(ClaimError::SwapExists(quote_hash));
    }
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    let config = config::get();
    ensure_claimable(&quote, now.as_secs(), config.permit_deadline)?;
    ensure_rail_is_enabled(&config, &quote)?;
    ensure_refundable(&quote)?;
    if let Some(party) = sanctioned_party(&quote) {
        return Err(ClaimError::Sanctioned { party });
    }
    let token = rail_source_token(&config, &quote)?;
    inflight::take(quote_hash, InFlightKind::Claim, now).map_err(ClaimError::InFlight)?;
    let verified = deposits::verify_evm_deposit(&DepositRead {
        chain_id: quote.src_chain,
        quote_hash,
        wanted: wanted(&quote, token),
        not_before: None,
    })
    .await;
    // the message chain ends here whatever the read said: the marker was for the outcall
    inflight::release(quote_hash);
    let deposit = verified?;
    if is_sanctioned(&party(deposit.from)) {
        return Err(ClaimError::Sanctioned { party: "from" });
    }
    // the halt can land while the read is out, and a line appended after it would create a
    // swap the operator believes is not there; re-read with no await before the append
    require_not_halted().map_err(ClaimError::Guard)?;
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
    let token = quote.evm_address(QuoteAddressField::SrcToken)?;
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
            ClaimError::RailUnavailable { rail } => Self::RailUnavailable {
                rail: rail.to_string(),
            },
            ClaimError::RailToken(error) => Self::RailToken(error.into()),
            ClaimError::QuoteAddress(error) => Self::QuoteAddress(error.into()),
            ClaimError::Deposit(error) => Self::Deposit(error.into()),
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
            PullError::QuoteAddress(error) => Self::QuoteAddress(error.into()),
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
            DepositError::RangeTooWide {
                from,
                anchor,
                windows,
                cap,
            } => Self::RangeTooWide {
                from: from.get(),
                anchor: anchor.get(),
                windows,
                cap,
            },
            DepositError::Rpc(error) => Self::Rpc(error.into()),
            DepositError::UnreadableHead => Self::UnreadableHead,
            DepositError::UnreadableLogs => Self::UnreadableLogs,
            DepositError::NotFound { quote_hash } => Self::NotFound {
                quote_hash: quote_hash.into_bytes(),
            },
            DepositError::NoneMatches { quote_hash, seen } => Self::NoneMatches {
                quote_hash: quote_hash.into_bytes(),
                seen,
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
