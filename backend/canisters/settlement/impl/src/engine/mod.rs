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

use crate::deposits::{self, DepositError, DepositRead, VaultError, Wanted, WantedAmount};
use crate::guards::require_not_halted;
use crate::mints::{self, MintError};
use crate::rails::{self, Position, RailError, RailStep, RailTx, ReclaimStep};
use crate::state::Store;
use crate::storage::events::{append_event, read_state, AppendError};
use crate::storage::sanctions::is_sanctioned;
use crate::storage::{attestations, config, ecdsa_address, eco_intents, engine_cursor};
use crate::tx::{self, TxError};
use std::cell::Cell;
use thiserror::Error;
use types::abi::{vault_payout, vault_refund};
use types::events::TxPurpose;
use types::quote::{QuoteAddressError, QuoteAddressField, QuoteError};
use types::{
    BasisPoints, ChainId, EventType, EvmAddress, GasAmount, Leg, Outcome, Quote, QuoteHash, Swap,
    SwapStatus, Timestamp, TokenAmount, TxHash, Wei,
};

/// The gas a vault payout needs: one transfer and the vault's event.
pub const PAYOUT_GAS_LIMIT: GasAmount = GasAmount::new(120_000);

/// The gas a vault refund needs: the same shape as a payout.
pub const REFUND_GAS_LIMIT: GasAmount = GasAmount::new(120_000);

/// The most swaps one tick acts on. A tick awaits a signature per transaction it sends,
/// so an unbounded tick under a backlog would run for minutes; the rest wait for the next
/// tick, which starts where this one stopped. A config knob when the engine's other
/// windows become one.
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
    let leg = swap.last_leg.zip(swap.last_outcome);
    // one arm per status, and the closed ones first: a swap that is already stopped is
    // waited on whatever its latest leg says, including the half-recorded leg below
    match (swap.status, leg) {
        (WaitingForUser | Done | Refunded | Frozen, _) => Action::Wait,
        // a leg that closed with no outcome is a fold no line produces
        _ if swap.last_leg.is_some() && swap.last_outcome.is_none() => {
            Action::Freeze("a leg closed with no outcome recorded")
        }
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
    #[error("the payout leg carries no amount this canister recorded")]
    NoPayoutRecorded,
    #[error("the payout of {paid_out} is above the {paid} the stable brought in")]
    PayoutAboveStable {
        paid_out: TokenAmount,
        paid: TokenAmount,
    },
    #[error("the canister was halted while the read was out, so nothing was recorded")]
    Halted,
    #[error("the quote names no refund address")]
    NoRefundAddress,
    #[error(transparent)]
    QuoteAddress(#[from] QuoteAddressError),
    #[error(transparent)]
    Tx(#[from] TxError),
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Deposit(#[from] DepositError),
    #[error(transparent)]
    Mint(#[from] MintError),
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

/// The swaps this tick acts on: at most `cap` of them, in swap id order, starting after
/// `after` and wrapping at the end of the map.
///
/// The cursor is a place in the order and not a swap, so one naming a swap that has since
/// closed starts the window at the next id after it. Without the rotation a backlog of
/// more than `cap` open swaps would hand every tick the same low ids, and a swap whose
/// action is refused every tick (its chain's data stale, a knob its rail needs unset)
/// would hold its slot forever: this way a refused swap is passed over until the window
/// comes round again.
fn window(
    open: &[(QuoteHash, Swap)],
    after: Option<QuoteHash>,
    cap: usize,
) -> Vec<(QuoteHash, Swap)> {
    let start = match after {
        None => 0,
        Some(after) => open.partition_point(|(quote_hash, _)| *quote_hash <= after),
    };
    open.iter()
        .skip(start)
        .chain(open.iter().take(start))
        .take(cap)
        .cloned()
        .collect()
}

/// One tick over the open swaps: decides and acts on each in turn, at most
/// [`MAX_SWAPS_PER_TICK`] of them, starting after the swap the last tick ended on and
/// wrapping, so no swap waits behind another for good. Nothing while halted, and nothing
/// while another tick is still running.
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
            .collect()
    });
    for (quote_hash, swap) in window(&open, engine_cursor::get(), MAX_SWAPS_PER_TICK) {
        // the halt can land during an await; the cursor already names the last swap this
        // tick drove, so the tick after the halt lifts begins with the one it never
        // reached instead of driving the earlier ones again
        if require_not_halted().is_err() {
            return drive;
        }
        // every swap the tick considers moves the cursor past it, whatever its action
        // came to: a swap whose action is refused must not pin the window to itself
        engine_cursor::set(quote_hash);
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

/// Nobody on the sanctions set is paid. The set is checked at the claim, and again here,
/// where the money actually leaves: an address listed while a swap was executing (minutes
/// on CCTP) would otherwise be paid by a canister that screened it only at intake. A hit
/// freezes the swap with the party named, so the funds stay in the vault for an operator.
fn ensure_unsanctioned(to: EvmAddress, party: &'static str) -> Result<(), &'static str> {
    if is_sanctioned(&types::Address::from(to)) {
        return Err(party);
    }
    Ok(())
}

/// The quote a swap is for, read off its own bytes.
fn quote_of(swap: &Swap) -> Result<Quote, EngineError> {
    Ok(Quote::parse(&swap.quote_bytes)?)
}

/// What a swap is to be paid out: the stable less the platform's fee as the config reads
/// now, held to the quote. Priced once, where the payout is decided; what the record says
/// afterwards comes from [`recorded_payout`] and never from here again.
fn payout_for(swap: &Swap, quote: &Quote) -> Result<Payout, EngineError> {
    let paid = swap.amount_paid.ok_or(EngineError::NoAmountPaid)?;
    payout_of(paid, config::get().platform_fee, quote.min_out)
}

/// What the swap's own payout leg was signed for, and the fee it therefore left in the
/// vault: the stable that arrived less the amount the payout pays out. Read off the fold,
/// so a config the operator moved between the send and its confirmation changes nothing
/// in the record, and a fee the live config would now refuse cannot leave a confirmed
/// payout unrecorded.
fn recorded_payout(swap: &Swap) -> Result<Payout, EngineError> {
    let paid = swap.amount_paid.ok_or(EngineError::NoAmountPaid)?;
    let amount = swap.paid_out.ok_or(EngineError::NoPayoutRecorded)?;
    let fee = paid
        .checked_sub(amount)
        .ok_or(EngineError::PayoutAboveStable {
            paid_out: amount,
            paid,
        })?;
    Ok(Payout { amount, fee })
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
    let token = quote.evm_address(QuoteAddressField::DstToken)?;
    let to = quote.evm_address(QuoteAddressField::DstAddress)?;
    if let Err(party) = ensure_unsanctioned(to, "dst_address") {
        return freeze(quote_hash, &format!("the quote's {party} is sanctioned"));
    }
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
    if quote.refund_address.is_none() {
        return freeze(quote_hash, "the quote names no refund address");
    }
    let to = quote.evm_address(QuoteAddressField::RefundAddress)?;
    if let Err(party) = ensure_unsanctioned(to, "refund_address") {
        return freeze(quote_hash, &format!("the quote's {party} is sanctioned"));
    }
    // the fold's token names the same token as the quote's (the guard holds the line to
    // it), so the refund is of the quote's token as the quote spells it
    let token = quote.evm_address(QuoteAddressField::SrcToken)?;
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

/// The payout landed: the platform's fee, if there is one, is accrued, and then the swap
/// is done. The fee comes from the payout the swap itself sent, so nothing here can
/// refuse on a config that moved; the fee line goes first, because a refused line behind
/// a closed swap would be lost and nothing retries it. A payout leg the fold holds no
/// amount for is a fold no line produces, so the swap stops for a human rather than
/// looping. The inboxes are done with the swap.
fn record_done(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let payout = match recorded_payout(swap) {
        Ok(payout) => payout,
        Err(error @ (EngineError::NoPayoutRecorded | EngineError::PayoutAboveStable { .. })) => {
            return freeze(quote_hash, &error.to_string())
        }
        Err(error) => return Err(error),
    };
    if payout.fee > TokenAmount::ZERO {
        append_event(EventType::FeeAccrued {
            quote_hash,
            amount: payout.fee,
        })?;
    }
    append_event(EventType::SwapDone { quote_hash })?;
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
fn at_the_rail<Step>(
    quote_hash: QuoteHash,
    swap: &Swap,
    f: impl FnOnce(&dyn rails::CallRail, &Position) -> Result<Step, RailError>,
) -> Result<(Quote, Step), EngineError> {
    let quote = quote_of(swap)?;
    let config = config::get();
    let mine = ecdsa_address::get().ok_or(EngineError::AddressNotDerived)?;
    let attestation = attestations::get(quote_hash);
    let intent = eco_intents::get(quote_hash);
    let now = Timestamp::from_nanos(ic_cdk::api::time()).as_secs();
    let at = Position {
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
    let step = f(rail.as_ref(), &at)?;
    Ok((quote, step))
}

/// The rail's move for a swap that is executing.
async fn rail_step(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let (quote, step) = at_the_rail(quote_hash, swap, |rail, at| rail.step(at))?;
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
        RailStep::ReadMint { chain_id, tx_hash } => read_mint(quote_hash, chain_id, tx_hash).await,
        RailStep::Stuck(reason) => freeze(quote_hash, reason),
    }
}

/// Reads what the rail's mint delivered to the destination vault, off the mint's own
/// receipt, and records it as the stable arriving: what the chain says arrived, never what
/// the burn promised. The inbox is done with the attestation once the mint has delivered.
// todo_harden_reads: the read is the mint read, single and unreplicated, bound to a hash
// this canister signed, the configured messenger, vault and USDC, and the configured depth.
async fn read_mint(
    quote_hash: QuoteHash,
    chain_id: ChainId,
    tx_hash: TxHash,
) -> Result<Did, EngineError> {
    let minted = mints::read_mint(chain_id, tx_hash).await?;
    // the halt can land while the read is out, the same rule as every other append after
    // an await
    require_not_halted().map_err(|_| EngineError::Halted)?;
    append_event(EventType::PaidInStable {
        quote_hash,
        chain_id,
        amount: minted.amount,
    })?;
    attestations::remove(quote_hash);
    Ok(Did::Recorded)
}

/// Reads the destination vault for the rail's fill: a deposit for the quote, in the token
/// the user is paid, of at least the least the user was quoted, deep enough, is the stable
/// arriving. A deposit of another token or of less under the hash is somebody else's and
/// not the fill. Nothing there past the rail's deadline turns the swap into a refund;
/// nothing there before it is waited on.
// todo_harden_reads: the read is the deposit read, single and unreplicated, bound to the
// destination vault, the quote, the token and amount wanted, and the configured depth.
async fn check_arrival(
    quote_hash: QuoteHash,
    quote: &Quote,
    chain_id: ChainId,
    expired: bool,
) -> Result<Did, EngineError> {
    let token = quote.evm_address(QuoteAddressField::DstToken)?;
    let read = DepositRead {
        chain_id,
        quote_hash,
        wanted: Wanted {
            token,
            amount: WantedAmount::AtLeast(quote.min_out),
        },
        not_before: None,
    };
    let read = deposits::verify_evm_deposit(&read).await;
    // the halt can land while the read is out, and a line appended after it would move a
    // swap the operator believes is stopped; re-read with no await before the append
    require_not_halted().map_err(|_| EngineError::Halted)?;
    match read {
        Ok(deposit) => {
            append_event(EventType::PaidInStable {
                quote_hash,
                chain_id,
                amount: deposit.amount,
            })?;
            Ok(Did::Recorded)
        }
        Err(DepositError::NotFound { .. } | DepositError::NoneMatches { .. }) if expired => {
            append_event(EventType::RefundStarted {
                quote_hash,
                reason: "the intent expired with nothing delivered".to_string(),
            })?;
            Ok(Did::Recorded)
        }
        Err(DepositError::NotFound { .. } | DepositError::NoneMatches { .. }) => Ok(Did::Nothing),
        Err(error) => Err(error.into()),
    }
}

/// The rail's move for a swap being refunded whose funds are on the rail.
async fn reclaim(quote_hash: QuoteHash, swap: &Swap) -> Result<Did, EngineError> {
    let (_, step) = at_the_rail(quote_hash, swap, |rail, at| rail.reclaim(at))?;
    match step {
        ReclaimStep::Send(tx) => send(tx).await,
        ReclaimStep::Wait(_) => Ok(Did::Nothing),
        ReclaimStep::Stuck(reason) => freeze(quote_hash, reason),
    }
}
