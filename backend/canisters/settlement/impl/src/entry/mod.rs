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
use crate::guards::{require_not_halted, require_quoter, require_quoter_watcher_or_controller};
use crate::state::{pending_quotes, Store};
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::sanctions::is_sanctioned;
use crate::storage::{config, inflight, outbox};
use crate::tx::{self, TxError};
use settlement_api::types::entry::{PermitError, PullRequest, MAX_PERMIT2_SIGNATURE_BYTES};
use settlement_api::types::errors::GuardError;
use settlement_api::types::wire_len;
use std::time::Duration;
use thiserror::Error;
use types::abi::{vault_pull_with_permit2, Permit2Permit};
use types::events::TxPurpose;
use types::quote::QuoteError;
use types::quote::{QuoteAddressError, QuoteAddressField};
use types::rail::{ensure_rail_tokens, RailTokenError};
use types::{
    Address, Config, EventType, EvmAddress, GasAmount, GasMode, InFlight, InFlightKind, Quote,
    QuoteHash, Rail, Timestamp, TokenAmount, TxHash, UnixSeconds, Wei,
};

/// The gas a `pullWithPermit2` needs: Permit2's signature and witness check and the
/// storage write that spends its nonce, the transfer, and the vault's own balance
/// measurement and event, with room over the measured cost.
const PULL_GAS_LIMIT: GasAmount = GasAmount::new(200_000);

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
    #[error("quote {0} is not one the quoter registered")]
    NotRegistered(QuoteHash),
    #[error(transparent)]
    RailToken(#[from] RailTokenError),
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
    #[error(transparent)]
    Deposit(#[from] DepositError),
    #[error(transparent)]
    Append(#[from] AppendError),
}

/// A permit as the vault's `pullWithPermit2` takes it, in the domain's own types, with
/// the two fields the calldata does not carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullPermit {
    /// the quote the signature's witness names
    pub witness: QuoteHash,
    pub owner: EvmAddress,
    /// the spender the signature names
    pub spender: EvmAddress,
    pub permit: Permit2Permit,
    pub signature: Vec<u8>,
}

impl TryFrom<PullRequest> for PullPermit {
    type Error = PermitError;

    fn try_from(request: PullRequest) -> Result<Self, Self::Error> {
        let permit = match request {
            PullRequest::Eip2612(_) => return Err(PermitError::NotAPermit2Permit),
            PullRequest::Permit2(permit) => permit,
        };
        if permit.signature.len() > MAX_PERMIT2_SIGNATURE_BYTES {
            return Err(PermitError::SignatureTooLong {
                len: wire_len(permit.signature.len()),
                cap: wire_len(MAX_PERMIT2_SIGNATURE_BYTES),
            });
        }
        let address = |field: &str, text: &str| {
            text.parse().map_err(
                |reason: types::evm::EvmAddressError| PermitError::NotAnAddress {
                    field: field.to_string(),
                    reason: reason.into(),
                },
            )
        };
        Ok(Self {
            witness: QuoteHash::new(permit.quote_hash),
            owner: address("owner", &permit.owner)?,
            spender: address("spender", &permit.spender)?,
            permit: Permit2Permit {
                token: address("token", &permit.token)?,
                amount: TokenAmount::from_canonical_nat(permit.amount)
                    .ok_or(PermitError::AmountTooLarge)?,
                nonce: types::Permit2Nonce::try_from(permit.nonce)
                    .map_err(|_| PermitError::NonceTooLarge)?,
                deadline: UnixSeconds::new(permit.deadline_s),
            },
            signature: permit.signature,
        })
    }
}

/// Why a permit the caller sent is not the one this quote's pull may use, with what it
/// says and what the quote says beside it.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PermitMismatch {
    #[error("the signature's witness names quote {signed_for}, and not this one")]
    Witness { signed_for: QuoteHash },
    #[error("the permit is for token {permitted}, and the quote's is {wanted}")]
    Token {
        permitted: EvmAddress,
        wanted: EvmAddress,
    },
    #[error("the permit is for {permitted}, and the quote's amount is {wanted}")]
    Amount {
        permitted: TokenAmount,
        wanted: TokenAmount,
    },
    #[error("the signature names spender {signed_for}, and the vault is {vault}")]
    Spender {
        signed_for: EvmAddress,
        vault: EvmAddress,
    },
    #[error("the permit's own deadline passed at {deadline}; it is {now}")]
    Expired {
        deadline: UnixSeconds,
        now: UnixSeconds,
    },
}

impl From<PermitMismatch> for settlement_api::types::entry::PermitMismatch {
    fn from(mismatch: PermitMismatch) -> Self {
        match mismatch {
            PermitMismatch::Witness { signed_for } => Self::Witness {
                signed_for: signed_for.into_bytes(),
            },
            PermitMismatch::Token { permitted, wanted } => Self::Token {
                permitted: permitted.to_string(),
                wanted: wanted.to_string(),
            },
            PermitMismatch::Amount { permitted, wanted } => Self::Amount {
                permitted: permitted.into(),
                wanted: wanted.into(),
            },
            PermitMismatch::Spender { signed_for, vault } => Self::Spender {
                signed_for: signed_for.to_string(),
                vault: vault.to_string(),
            },
            PermitMismatch::Expired { deadline, now } => Self::Expired {
                deadline_s: deadline.get(),
                now_s: now.get(),
            },
        }
    }
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
    #[error("the permit is not this quote's: {0}")]
    PermitMismatch(#[from] PermitMismatch),
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
    #[error("the {rail} rail is not available on this deploy")]
    RailUnavailable { rail: Rail },
    #[error(transparent)]
    RailToken(#[from] RailTokenError),
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

/// The rail a quote names, if this deploy does not run it. The Eco rail is off until its
/// route is designed (see `rails::eco`).
pub fn unavailable_rail(config: &Config, quote: &Quote) -> Option<Rail> {
    match quote.rail {
        Rail::Eco if !config.eco_enabled.is_on() => Some(quote.rail),
        Rail::Eco | Rail::CctpV2Fast | Rail::CctpV2Standard => None,
    }
}

/// The rail a quote names has to be one this deploy runs, so a quote naming the Eco rail
/// while it is off is refused here, before an outcall is bought, rather than becoming a
/// swap whose funds the canister cannot move.
pub fn ensure_rail_is_enabled(config: &Config, quote: &Quote) -> Result<(), ClaimError> {
    match unavailable_rail(config, quote) {
        Some(rail) => Err(ClaimError::RailUnavailable { rail }),
        None => Ok(()),
    }
}

/// A quote has to name a refund address a refund can be paid to. The fold does not hold
/// the payer (`FundsReceived` carries none and its layout is frozen), so the refund
/// address is the only way the user's funds come back: a swap without one can only be
/// frozen with the funds in the vault, so no swap is created for a quote that has none.
pub fn ensure_refundable(quote: &Quote) -> Result<(), ClaimError> {
    quote.evm_address(QuoteAddressField::RefundAddress)?;
    Ok(())
}

/// A quote has to name a destination a payout can be sent to. The payout is the swap's
/// last leg, after the burn and the mint, so a destination that is no address would be
/// found out only once the funds had crossed, and retried on every tick after; it is
/// refused here instead, before an outcall is bought. Every chain this canister pays is an
/// EVM chain, through its vault there, so the destination must be an EVM address.
pub fn ensure_payable(quote: &Quote) -> Result<(), ClaimError> {
    quote.evm_address(QuoteAddressField::DstAddress)?;
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
        token: deposit.token.into(),
        amount: deposit.amount,
        tx_ref: format!("0x{}", hex::encode(deposit.tx_ref.as_ref())),
    }
}

/// An address as the sanctions set reads it.
fn party(address: EvmAddress) -> Address {
    address.into()
}

/// Claims the deposit a user made for `quote` and creates its swap.
///
/// Money-first, in this order: the halt switch and the caller, the quote itself, the swap
/// not existing yet, the quote still claimable, its rail on, its refund and destination
/// addresses payable, nobody it pays to sanctioned, its tokens the rail's; then the marker
/// (A8), then the read, which looks for the quote's token in exactly its amount among the
/// logs under the hash; then the payer is held to the sanctions set, and `FundsReceived`
/// is appended, which the fold checks again (A2). A refusal anywhere stores nothing.
///
/// The read starts at the height the quote was registered at, when the store kept one,
/// and reads one window where it would read a day of them. That height is the watcher's
/// head, which can run ahead of the chain, so it is where the read starts and never a
/// floor: when nothing at all matches above it, the plain lookback is read before the
/// claim is refused, and no height a watcher pushed puts a deposit out of reach.
pub async fn claim_swap(quote: Quote) -> Result<QuoteHash, ClaimError> {
    require_not_halted().map_err(ClaimError::Guard)?;
    require_quoter_watcher_or_controller().map_err(ClaimError::Guard)?;
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
    ensure_payable(&quote)?;
    if let Some(party) = sanctioned_party(&quote) {
        return Err(ClaimError::Sanctioned { party });
    }
    let token = rail_source_token(&config, &quote)?;
    // a swap's economics are the quoter's: only a quote the store holds is claimed, and a
    // controller stands in where a deposit has to be claimed by hand
    let pending = pending_quotes::get_pending(&quote_hash);
    if pending.is_none() && !ic_cdk::api::is_controller(&ic_cdk::api::caller()) {
        return Err(ClaimError::NotRegistered(quote_hash));
    }
    inflight::take(quote_hash, InFlightKind::Claim, now).map_err(ClaimError::InFlight)?;
    let read = |not_before| DepositRead {
        chain_id: quote.src_chain,
        quote_hash,
        wanted: wanted(&quote, token),
        not_before,
    };
    // the deposit that pays a quote is not in a block before the quote was registered
    let registered_at = pending.and_then(|entry| entry.registered_at);
    let mut verified = deposits::verify_evm_deposit(&read(registered_at)).await;
    if registered_at.is_some() && verified.as_ref().is_err_and(DepositError::found_nothing) {
        verified = deposits::verify_evm_deposit(&read(None)).await;
    }
    // the message chain ends here whatever the reads said: the marker was for the outcalls
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

/// The permit has to be this quote's own, in every field the user signed.
///
/// The witness is the quote hash, so a signature made for one quote frees nothing under
/// another; the token and the amount are the quote's, so the vault pulls what the swap is
/// for and no more; the spender is the vault the pull would call, which is the address
/// Permit2 takes from its caller; and the deadline has not passed, so the pull is not gas
/// spent on a signature Permit2 will refuse.
pub fn ensure_permit_binds(
    quote_hash: QuoteHash,
    quote: &Quote,
    token: EvmAddress,
    vault: EvmAddress,
    now: UnixSeconds,
    permit: &PullPermit,
) -> Result<(), PullError> {
    if permit.witness != quote_hash {
        return Err(PermitMismatch::Witness {
            signed_for: permit.witness,
        }
        .into());
    }
    if permit.permit.token != token {
        return Err(PermitMismatch::Token {
            permitted: permit.permit.token,
            wanted: token,
        }
        .into());
    }
    if permit.permit.amount != quote.amount_in {
        return Err(PermitMismatch::Amount {
            permitted: permit.permit.amount,
            wanted: quote.amount_in,
        }
        .into());
    }
    if permit.spender != vault {
        return Err(PermitMismatch::Spender {
            signed_for: permit.spender,
            vault,
        }
        .into());
    }
    if now > permit.permit.deadline {
        return Err(PermitMismatch::Expired {
            deadline: permit.permit.deadline,
            now,
        }
        .into());
    }
    Ok(())
}

/// Pulls a gasless user's funds into the vault with the permit they signed, and answers the
/// hash of the transaction that does it.
///
/// Pre-money: the quote must be pending and gasless, still payable, on a rail the deploy
/// runs and naming that rail's tokens, the permit its own, and nobody it involves
/// sanctioned; then the marker (A8), then the one send path, which allocates, signs,
/// records `PullSigned` and queues. The deposit the transaction makes is what `claim_swap`
/// then verifies, so nothing here creates a swap, and every refusal the claim makes on the
/// quote alone is made here first (rule A5): a pull the claim then refused would leave the
/// user's funds in the vault under a quote that never becomes a swap.
pub async fn start_gasless_pull(
    quote_hash: QuoteHash,
    permit: PullPermit,
) -> Result<TxHash, PullError> {
    require_not_halted().map_err(PullError::Guard)?;
    require_quoter().map_err(PullError::Guard)?;
    let quote = pending_quotes::quote_of(&quote_hash).ok_or(PullError::UnknownQuote(quote_hash))?;
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
    if let Some(rail) = unavailable_rail(&config, &quote) {
        return Err(PullError::RailUnavailable { rail });
    }
    ensure_rail_tokens(&config.usdc_addresses, &quote)?;
    let token = quote.evm_address(QuoteAddressField::SrcToken)?;
    let vault = deposits::vault_of(&config, quote.src_chain)?;
    ensure_permit_binds(quote_hash, &quote, token, vault, now.as_secs(), &permit)?;
    if is_sanctioned(&party(permit.owner)) {
        return Err(PullError::Sanctioned { party: "owner" });
    }
    if let Some(party) = sanctioned_party(&quote) {
        return Err(PullError::Sanctioned { party });
    }
    // a second pull while the first is still on its way would only revert on the vault,
    // which marks a quote per payer, and pay gas for it
    if let Some(out) = outbox::find(TxPurpose::GaslessPull(quote_hash)) {
        return Err(PullError::AlreadyPulling {
            tx_hash: out.tx_hash(),
        });
    }
    inflight::take(quote_hash, InFlightKind::Pull, now).map_err(PullError::InFlight)?;
    let sent = tx::create_and_send(
        TxPurpose::GaslessPull(quote_hash),
        quote.src_chain,
        vault,
        Wei::ZERO,
        vault_pull_with_permit2(quote_hash, permit.owner, &permit.permit, &permit.signature),
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
                kind: marker.kind.into(),
                since_ns: marker.since.as_nanos(),
            },
            ClaimError::RailUnavailable { rail } => Self::RailUnavailable {
                rail: rail.to_string(),
            },
            ClaimError::NotRegistered(quote_hash) => Self::NotRegistered(quote_hash.into_bytes()),
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
            PullError::PermitMismatch(mismatch) => Self::PermitMismatch(mismatch.into()),
            PullError::QuoteAddress(error) => Self::QuoteAddress(error.into()),
            PullError::Sanctioned { party } => Self::Sanctioned {
                party: party.to_string(),
            },
            PullError::InFlight(marker) => Self::InFlight {
                kind: marker.kind.into(),
                since_ns: marker.since.as_nanos(),
            },
            PullError::AlreadyPulling { tx_hash } => Self::AlreadyPulling {
                tx_hash: tx_hash.into_bytes(),
            },
            PullError::Vault(error) => Self::Vault(error.into()),
            PullError::Tx(error) => Self::Tx(error.into()),
            PullError::RailUnavailable { rail } => Self::RailUnavailable {
                rail: rail.to_string(),
            },
            PullError::RailToken(error) => Self::RailToken(error.into()),
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
            DepositError::NoDepth(crate::tx::NoDepth { chain_id }) => Self::NoDepth {
                chain_id: chain_id.get(),
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
