//! The swap engine: what happens to a swap between the lines the entry doors and the
//! outbox write.
//!
//! One pure decision, [`next_action`], reads a swap as the fold holds it and answers the
//! one thing to do next; [`drive_all`] runs it over every open swap on a timer, one action
//! per swap per tick, and does the sending, reading and appending the action asks for.
//! Every transaction goes through `tx::create_and_send`, so rules A4 to A6 hold here by
//! construction, and every line goes through `append_event`, whose guard is the authority
//! (A2). The rail the quote names is the one that runs, and no other.

#[cfg(test)]
mod tests;

use crate::deposits::{self, DepositError, VaultError};
use crate::guards::require_not_halted;
use crate::rails::{self, Leg as RailLeg, RailError, RailStep, RailTx};
use crate::state::Store;
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::{attestations, config, ecdsa_address, eco_intents};
use crate::tx::{self, TxError};
use std::cell::Cell;
use thiserror::Error;
use types::abi::{vault_payout, vault_refund};
use types::events::TxPurpose;
use types::quote::QuoteError;
use types::{
    BasisPoints, ChainId, EventType, EvmAddress, GasAmount, Leg, Outcome, Quote, QuoteHash, Swap,
    SwapStatus, Timestamp, TokenAmount, Wei,
};

/// The gas a vault payout needs: one transfer and the vault's event.
pub const PAYOUT_GAS_LIMIT: GasAmount = GasAmount::new(120_000);

/// The gas a vault refund needs: the same shape as a payout.
pub const REFUND_GAS_LIMIT: GasAmount = GasAmount::new(120_000);

/// The most swaps one tick acts on. A tick awaits a signature per transaction it sends,
/// so an unbounded tick under a backlog would run for minutes; the rest wait for the next
/// tick, oldest swap id first. A config knob when the engine's other windows become one.
pub const MAX_SWAPS_PER_TICK: usize = 50;

/// What the engine does for one swap on one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The rail's next move: a burn, a publish, a mint, or a read of the destination.
    Rail,
    /// The stable is on the destination side: pay the user out.
    SendPayout,
    /// The funds are in the source vault: pay the user back.
    SendRefund,
    /// The funds are on the rail: ask the rail whether they come back.
    RailReclaim,
    /// The payout landed.
    RecordDone,
    /// The refund landed.
    RecordRefunded,
    /// Nothing moved and the swap cannot go on: turn it into a refund.
    StartRefund(&'static str),
    /// No line leads from here: stop the swap for a human.
    Freeze(&'static str),
    /// Nothing to do: an attempt is open, the user is being asked, or the swap is closed.
    Wait,
}

/// The one decision: what follows from where a swap is. Pure, and the whole of it is the
/// table in the tests.
///
/// An open attempt is the outbox's to decide, so it is waited on whatever else the swap
/// says. Otherwise the status says which side of the rail the funds are on, and the latest
/// leg and its outcome say what just happened there.
pub fn next_action(swap: &Swap) -> Action {
    use SwapStatus::*;
    if swap.open_attempt.is_some() {
        return Action::Wait;
    }
    if matches!(swap.status, WaitingForUser | Done | Refunded | Frozen) {
        return Action::Wait;
    }
    // a leg that closed with no outcome is a fold no line produces
    if swap.last_leg.is_some() && swap.last_outcome.is_none() {
        return Action::Freeze("a leg closed with no outcome recorded");
    }
    let leg = swap.last_leg.zip(swap.last_outcome);
    match (swap.status, leg) {
        (FundsReceived, _) | (Executing, None) => Action::Rail,
        (Executing, Some((Leg::Burn | Leg::Mint, Outcome::Confirmed))) => Action::Rail,
        (Executing, Some((Leg::Burn, Outcome::Failed))) => {
            Action::StartRefund("the burn reverted on the chain")
        }
        (Executing, Some((Leg::Mint, Outcome::Failed))) => {
            Action::Freeze("the mint reverted on the chain")
        }
        (Executing, Some((Leg::Payout, Outcome::Confirmed))) => {
            Action::Freeze("a payout confirmed while executing")
        }
        (Executing, Some((Leg::Refund, Outcome::Confirmed))) => {
            Action::Freeze("a refund confirmed while executing")
        }
        (Executing, Some((Leg::Reclaim, Outcome::Confirmed))) => {
            Action::Freeze("a reclaim confirmed while executing")
        }
        (Executing, Some((Leg::Payout, Outcome::Failed))) => {
            Action::Freeze("a payout failed while executing")
        }
        (Executing, Some((Leg::Refund, Outcome::Failed))) => {
            Action::Freeze("a refund failed while executing")
        }
        (Executing, Some((Leg::Reclaim, Outcome::Failed))) => {
            Action::Freeze("a reclaim failed while executing")
        }
        (PaidInStable, _) => Action::SendPayout,
        (Delivering, Some((Leg::Payout, Outcome::Confirmed))) => Action::RecordDone,
        (Delivering, Some((Leg::Payout, Outcome::Failed))) => {
            Action::Freeze("the payout reverted on the chain")
        }
        (Delivering, _) => Action::Freeze("delivering with no payout signed"),
        (Refunding, None) => Action::SendRefund,
        (Refunding, Some((Leg::Burn, Outcome::Failed))) => Action::SendRefund,
        (Refunding, Some((Leg::Reclaim, Outcome::Confirmed))) => Action::SendRefund,
        (Refunding, Some((Leg::Refund, Outcome::Confirmed))) => Action::RecordRefunded,
        (Refunding, Some((Leg::Burn, Outcome::Confirmed))) => Action::RailReclaim,
        (Refunding, Some((Leg::Refund, Outcome::Failed))) => {
            Action::Freeze("the refund reverted on the chain")
        }
        (Refunding, Some((Leg::Reclaim, Outcome::Failed))) => {
            Action::Freeze("the reclaim reverted on the chain")
        }
        (Refunding, Some((Leg::Mint, Outcome::Confirmed))) => {
            Action::Freeze("the stable is on the destination side")
        }
        (Refunding, Some((Leg::Mint, Outcome::Failed))) => {
            Action::Freeze("the funds left for the rail and never arrived")
        }
        (Refunding, Some((Leg::Payout, _))) => Action::Freeze("the user was paid out"),
        (WaitingForUser | Done | Refunded | Frozen, _) => Action::Wait,
    }
}

/// What the user is paid, and what the platform keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payout {
    pub amount: TokenAmount,
    pub fee: TokenAmount,
}

/// Why the engine could not act on a swap this tick. Counted, never fatal: the next tick
/// reads the swap again.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("the swap's quote bytes are not a quote this canister reads: {0}")]
    UnparseableQuote(#[from] QuoteError),
    #[error("this canister's address is not derived yet")]
    AddressNotDerived,
    #[error(transparent)]
    Rail(#[from] RailError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("the payout of {payout} is below the {min_out} the user was quoted")]
    BelowMinOut {
        payout: TokenAmount,
        min_out: TokenAmount,
    },
    #[error("the fee on {amount} does not fit an amount")]
    FeeOverflow { amount: TokenAmount },
    #[error("the swap was paid in stable with no amount recorded")]
    NoAmountPaid,
    #[error("the quote names no refund address")]
    NoRefundAddress,
    #[error("the quote's {field} is not an EVM address")]
    QuoteNotAnAddress { field: &'static str },
    #[error(transparent)]
    Tx(#[from] TxError),
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Deposit(#[from] DepositError),
}

/// The payout for a swap paid `paid` in stable: less the platform's `fee` of it, rounded
/// down in the platform's disfavour, and refused below `min_out`.
pub fn payout_of(
    paid: TokenAmount,
    fee: BasisPoints,
    min_out: TokenAmount,
) -> Result<Payout, EngineError> {
    let fee = fee
        .apply_to(paid)
        .ok_or(EngineError::FeeOverflow { amount: paid })?;
    let amount = paid
        .checked_sub(fee)
        .ok_or(EngineError::FeeOverflow { amount: paid })?;
    if amount < min_out {
        return Err(EngineError::BelowMinOut {
            payout: amount,
            min_out,
        });
    }
    Ok(Payout { amount, fee })
}

/// What one tick did. Returned rather than logged, like the sweep's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drive {
    /// transactions handed to the send path
    pub sent: usize,
    /// lines appended: arrivals, completions, refunds started or landed, freezes
    pub recorded: usize,
    /// swaps the tick could not act on, left for the next
    pub refused: usize,
    /// swaps read and left alone
    pub waited: usize,
}

thread_local! {
    // A7: set while a tick is running, cleared when its guard drops, including on a trap.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Held for the length of one tick, so two ticks can never overlap: a tick awaits
/// signatures and reads, and a second one reading the same swaps would send twice.
struct TickGuard;

impl TickGuard {
    fn take() -> Option<Self> {
        RUNNING.with(|running| {
            if running.get() {
                return None;
            }
            running.set(true);
            Some(Self)
        })
    }
}

impl Drop for TickGuard {
    fn drop(&mut self) {
        RUNNING.with(|running| running.set(false));
    }
}

/// One tick over every open swap: decides and acts on each in turn, at most
/// [`MAX_SWAPS_PER_TICK`] of them. Nothing while halted, and nothing while another tick is
/// still running.
pub async fn drive_all() -> Drive {
    let mut drive = Drive::default();
    let Some(_guard) = TickGuard::take() else {
        return drive;
    };
    if require_not_halted().is_err() {
        return drive;
    }
    let open: Vec<(QuoteHash, Swap)> = read_state(|state| {
        state
            .store()
            .swaps()
            .into_iter()
            .filter(|(_, swap)| next_action(swap) != Action::Wait)
            .take(MAX_SWAPS_PER_TICK)
            .collect()
    });
    for (quote_hash, swap) in open {
        // the halt can land during an await
        if require_not_halted().is_err() {
            return drive;
        }
        match drive_one(quote_hash, &swap).await {
            Ok(Did::Sent) => drive.sent += 1,
            Ok(Did::Recorded) => drive.recorded += 1,
            Ok(Did::Nothing) => drive.waited += 1,
            Err(_) => drive.refused += 1,
        }
    }
    drive
}

/// What one action came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Did {
    Sent,
    Recorded,
    Nothing,
}

/// Decides and acts on one swap.
async fn drive_one(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    match next_action(swap) {
        Action::Wait => Ok(Did::Nothing),
        Action::Freeze(reason) => freeze(quote_hash, reason),
        Action::StartRefund(reason) => {
            append_event(EventType::RefundStarted {
                quote_hash,
                reason: reason.to_string(),
            })?;
            Ok(Did::Recorded)
        }
        Action::RecordDone => record_done(quote_hash, swap),
        Action::RecordRefunded => record_refunded(quote_hash, swap),
        Action::SendPayout => send_payout(quote_hash, swap).await,
        Action::SendRefund => send_refund(quote_hash, swap).await,
        Action::Rail => rail_step(quote_hash, swap).await,
        Action::RailReclaim => reclaim(quote_hash, swap).await,
    }
}

fn freeze(quote_hash: QuoteHash, reason: &str) -> Result<Did, EngineError> {
    append_event(EventType::Frozen {
        quote_hash,
        reason: reason.to_string(),
    })?;
    forget(quote_hash);
    Ok(Did::Recorded)
}

/// Drops what the inboxes held for a swap that has closed.
fn forget(quote_hash: QuoteHash) {
    attestations::remove(quote_hash);
    eco_intents::remove(quote_hash);
}

/// The quote's `field` as an EVM address.
fn quote_address(text: &str, field: &'static str) -> Result<EvmAddress, EngineError> {
    text.parse()
        .map_err(|_| EngineError::QuoteNotAnAddress { field })
}

/// The quote a swap is for, read off its own bytes.
fn quote_of(swap: &Swap) -> Result<Quote, EngineError> {
    Ok(Quote::parse(&swap.quote_bytes)?)
}

/// What a swap is paid out: the stable less the platform's fee, held to the quote.
fn payout_for(swap: &Swap, quote: &Quote) -> Result<Payout, EngineError> {
    let paid = swap.amount_paid.ok_or(EngineError::NoAmountPaid)?;
    payout_of(paid, config::get().platform_fee, quote.min_out)
}

/// Hands one rail transaction to the send path.
async fn send(tx: RailTx) -> Result<Did, EngineError> {
    tx::create_and_send(
        tx.purpose,
        tx.chain_id,
        tx.to,
        tx.value,
        tx.data,
        tx.gas_limit,
    )
    .await?;
    Ok(Did::Sent)
}

/// The payout: from the destination vault, the quote's token to the quote's address, the
/// stable less the fee. A payout the quote refuses freezes the swap rather than paying
/// less than the user was promised.
async fn send_payout(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let quote = quote_of(swap)?;
    let payout = match payout_for(swap, &quote) {
        Ok(payout) => payout,
        Err(EngineError::BelowMinOut { .. }) => {
            return freeze(
                quote_hash,
                "the payout is below the least the user was quoted",
            )
        }
        Err(error) => return Err(error),
    };
    let token = quote_address(quote.dst_token.as_str(), "dst_token")?;
    let to = quote_address(quote.dst_address.as_str(), "dst_address")?;
    let vault = deposits::vault_of(&config::get(), quote.dst_chain)?;
    send(RailTx {
        purpose: TxPurpose::Payout(quote_hash),
        chain_id: quote.dst_chain,
        to: vault,
        value: Wei::ZERO,
        data: vault_payout(quote_hash, token, to, payout.amount),
        gas_limit: PAYOUT_GAS_LIMIT,
    })
    .await
}

/// The refund: from the source vault, the token that arrived to the quote's refund
/// address, the whole amount. A quote that names no refund address cannot be refunded by
/// this canister, which does not know the payer, and is frozen for a human.
async fn send_refund(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let quote = quote_of(swap)?;
    let Some(refund_address) = quote.refund_address.as_ref() else {
        return freeze(quote_hash, "the quote names no refund address");
    };
    let to = quote_address(refund_address.as_str(), "refund_address")?;
    let token = quote_address(swap.src_token.as_str(), "src_token")?;
    let vault = deposits::vault_of(&config::get(), quote.src_chain)?;
    send(RailTx {
        purpose: TxPurpose::Refund(quote_hash),
        chain_id: quote.src_chain,
        to: vault,
        value: Wei::ZERO,
        data: vault_refund(quote_hash, token, to, swap.amount_in),
        gas_limit: REFUND_GAS_LIMIT,
    })
    .await
}

/// The payout landed: the swap is done, and the platform's fee, if there is one, is
/// accrued. The inboxes are done with the swap.
fn record_done(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let quote = quote_of(swap)?;
    let payout = payout_for(swap, &quote)?;
    append_event(EventType::SwapDone { quote_hash })?;
    if payout.fee > TokenAmount::ZERO {
        append_event(EventType::FeeAccrued {
            quote_hash,
            amount: payout.fee,
        })?;
    }
    forget(quote_hash);
    Ok(Did::Recorded)
}

/// The refund landed. The inboxes are done with the swap.
fn record_refunded(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let quote = quote_of(swap)?;
    let to = quote.refund_address.ok_or(EngineError::NoRefundAddress)?;
    append_event(EventType::Refunded {
        quote_hash,
        chain_id: quote.src_chain,
        token: swap.src_token.clone(),
        amount: swap.amount_in,
        to,
    })?;
    forget(quote_hash);
    Ok(Did::Recorded)
}

/// Runs `f` with the rail's view of the swap: its quote, the config, this canister's
/// address, what the inboxes hold, and the clock.
fn with_rail_leg(
    quote_hash: QuoteHash,
    swap: &Swap,
    f: impl FnOnce(&dyn rails::CallRail, &RailLeg) -> Result<RailStep, RailError>,
) -> Result<(Quote, RailStep), EngineError> {
    let quote = quote_of(swap)?;
    let config = config::get();
    let mine = ecdsa_address::get().ok_or(EngineError::AddressNotDerived)?;
    let attestation = attestations::get(quote_hash);
    let intent = eco_intents::get(quote_hash);
    let now = Timestamp::from_nanos(ic_cdk::api::time()).as_secs();
    let leg = RailLeg {
        quote_hash,
        quote: &quote,
        swap,
        config: &config,
        mine,
        attestation: attestation.as_ref(),
        intent: intent.as_ref(),
        now,
    };
    let rail = rails::for_rail(quote.rail);
    let step = f(rail.as_ref(), &leg)?;
    Ok((quote, step))
}

/// The rail's move for a swap that is executing.
async fn rail_step(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let (quote, step) = with_rail_leg(quote_hash, swap, |rail, leg| rail.step(leg))?;
    match step {
        RailStep::Send(tx) => send(tx).await,
        RailStep::Wait(_) => Ok(Did::Nothing),
        RailStep::Arrived { chain_id, amount } => {
            append_event(EventType::PaidInStable {
                quote_hash,
                chain_id,
                amount,
            })?;
            // the mint is done with the attestation
            attestations::remove(quote_hash);
            Ok(Did::Recorded)
        }
        RailStep::CheckArrival { chain_id, expired } => {
            check_arrival(quote_hash, &quote, chain_id, expired).await
        }
        RailStep::Reclaim(tx) => send(tx).await,
        RailStep::Stuck(reason) => freeze(quote_hash, reason),
    }
}

/// Reads the destination vault for the rail's fill: a deposit for the quote, in the token
/// the user is paid, deep enough, is the stable arriving. Nothing there past the rail's
/// deadline turns the swap into a refund; nothing there before it is waited on.
// todo_harden_reads: the read is the deposit read, single and unreplicated, bound to the
// destination vault, the quote and the configured depth.
async fn check_arrival(
    quote_hash: QuoteHash,
    quote: &Quote,
    chain_id: ChainId,
    expired: bool,
) -> Result<Did, EngineError> {
    let token = quote_address(quote.dst_token.as_str(), "dst_token")?;
    match deposits::verify_evm_deposit(chain_id, quote_hash).await {
        Ok(deposit) if deposit.token == token => {
            append_event(EventType::PaidInStable {
                quote_hash,
                chain_id,
                amount: deposit.amount,
            })?;
            Ok(Did::Recorded)
        }
        // another token in the destination vault under this quote is not the fill
        Ok(_) | Err(DepositError::NotFound { .. }) if expired => {
            append_event(EventType::RefundStarted {
                quote_hash,
                reason: "the intent expired with nothing delivered".to_string(),
            })?;
            Ok(Did::Recorded)
        }
        Ok(_) | Err(DepositError::NotFound { .. }) => Ok(Did::Nothing),
        Err(error) => Err(error.into()),
    }
}

/// The rail's move for a swap being refunded whose funds are on the rail.
async fn reclaim(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let (_, step) = with_rail_leg(quote_hash, swap, |rail, leg| rail.reclaim(leg))?;
    match step {
        RailStep::Reclaim(tx) | RailStep::Send(tx) => send(tx).await,
        RailStep::Wait(_) | RailStep::CheckArrival { .. } => Ok(Did::Nothing),
        RailStep::Arrived { .. } => Ok(Did::Nothing),
        RailStep::Stuck(reason) => freeze(quote_hash, reason),
    }
}
