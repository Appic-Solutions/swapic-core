//! The Base payouts' queue, their timeouts and their undo (P11b Task 6), one pass after every
//! pool movement (the engine's drive) and every minute (the payout timer):
//!
//! - every ICP swap still on its way is driven: a pull whose answer was lost asked again (the
//!   same pull, within the ledger's deduplication window), its legs, an undo's refund;
//! - the queue is served first in first out: its head waits the moment the Base pocket's free
//!   USDC covers it, and nothing behind it moves ahead of it;
//! - a waiting payout that waited `order_wait` with no order is listed again, at the back of
//!   `payouts_waiting`;
//! - at its deadline a payout not signed is undone: an ICP swap refunds what it pulled on ICP,
//!   a withdrawal mints its shares back;
//! - a payout is signed once (2026-10-23): one whose transaction reverted is undone the same
//!   way, at once, but only once its transaction is closed at the crediting depth and Swapic
//!   says its order never ran (`used(id)` false, read by every node, `agreed::ran`), so a
//!   reorg that turned the revert into a payment is never paid twice; an order that ran after
//!   all stops the payout for a human;
//! - a payout whose order published an Eco intent is delivering until the intent's fill is
//!   read, once, past its route deadline (every node at one block whose time is past it,
//!   `agreed::fill_after`): filled, it is paid; never filled, it is undone at once, the
//!   user refunded, while its USDC comes back from Eco later (`PayoutRefunded`, credited to
//!   the Base pocket only).

#[cfg(test)]
mod tests;

use crate::pool::payout::{Busy, Ledgers, Stop, DEDUP_WINDOW};
use crate::pool::plan::PlanReads;
use crate::pool::{icp_swap, log};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use types::eco::Fill;
use types::events::TxPurpose;
use types::pool::payout::{EvmPayout, PayoutKind, PayoutSettings, PayoutStatus};
use types::pool::{evm_payout_ref, PoolStep};
use types::{BlockNumber, ChainId, Timestamp};

/// How long after a time the pass waits before it reads a chain for a block past it (the one
/// rule, [`crate::agreed::fill_wait`]): the payout's destination for its Eco intent's fill,
/// the pool chain for whether its reverted order ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Waits {
    pub fill: std::time::Duration,
    pub ran: std::time::Duration,
}

/// What the pass does with one payout now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing now.
    Wait,
    /// An ICP swap on its way: its own drive.
    Drive,
    /// Listed again: it waited `order_wait` with no order.
    Relist,
    /// A planned swap's plan tried again: an earlier try waited.
    Fit,
    /// Undone; `checked`: it was signed once, so only once its transaction is closed for good
    /// and Swapic says its order never ran.
    Undo { why: String, checked: bool },
    /// Its Eco intent's fill read on `chain`, at a block whose time is past `after_s`.
    ReadFill {
        chain: ChainId,
        intent: [u8; 32],
        after_s: u64,
    },
    /// Stopped for a human.
    Stop(String),
}

/// The pass's decision for `payout` at `now`, pure: `in_flight` says a transaction for it is
/// still being signed or is in the outbox; `waits` are its chains' [`Waits`].
pub fn next(
    payout: &EvmPayout,
    settings: &PayoutSettings,
    now: Timestamp,
    in_flight: bool,
    waits: Waits,
) -> Next {
    let late = now >= payout.terms.deadline;
    match &payout.status {
        // a pull too old to deduplicate can no longer be asked again safely: a human's
        PayoutStatus::Opened if now.saturating_duration_since(payout.opened_at) >= DEDUP_WINDOW => {
            Next::Stop("ALERT: the pull's outcome can no longer be asked for".to_string())
        }
        // a withdrawal's undo needs no ledger: its shares come back at once
        PayoutStatus::Undoing(why) if !payout.is_icp_swap() => Next::Undo {
            why: why.clone(),
            checked: false,
        },
        PayoutStatus::Opened | PayoutStatus::Swapping | PayoutStatus::Undoing(_) => Next::Drive,
        // the place in the queue is the serve's to change; the deadline undoes it
        PayoutStatus::Known | PayoutStatus::Queued if late => Next::Undo {
            why: "past its deadline".to_string(),
            checked: false,
        },
        PayoutStatus::Known => Next::Drive,
        PayoutStatus::Queued => Next::Wait,
        PayoutStatus::Waiting if in_flight => Next::Wait,
        // old semantics: a reverted payout waiting with its order pinned, to be signed again,
        // is undone only once Swapic says that order never ran. hfisl's real log (2,310
        // events, read 2026-10-22) holds no Pool event, so no such state exists on the
        // deployed canister: no migration
        PayoutStatus::Waiting if late => Next::Undo {
            why: "past its deadline".to_string(),
            checked: payout.order.is_some(),
        },
        PayoutStatus::Waiting if refittable(payout, now) => Next::Fit,
        PayoutStatus::Waiting => {
            let since = payout.waiting_since.unwrap_or(payout.opened_at);
            if now.saturating_duration_since(since) >= settings.order_wait {
                Next::Relist
            } else {
                Next::Wait
            }
        }
        // reverted: undone at once, its transaction closed for good and its order unused
        PayoutStatus::Reverted if in_flight => Next::Wait,
        // no block recorded for its revert: nothing can be read at or after it, a human's
        // (the review, 2026-10-24: once, not a log line every pass)
        PayoutStatus::Reverted if payout.reverted_block.is_none() => {
            Next::Stop(NO_REVERT_BLOCK.to_string())
        }
        // whether its order ran is read once an agreed block can be at or past its revert
        // (the review, 2026-10-24: not asked every pass before that)
        PayoutStatus::Reverted
            if payout
                .reverted_at
                .is_some_and(|at| now.saturating_duration_since(at) < waits.ran) =>
        {
            Next::Wait
        }
        PayoutStatus::Reverted => Next::Undo {
            why: REVERTED.to_string(),
            checked: true,
        },
        // its Eco intent's fill, read once the agreed block can be past the route's deadline,
        // and not again within one wait of a read too early
        PayoutStatus::Delivering => {
            let after_s = fill_read_after(payout);
            let now_s = now.as_secs().get();
            let wait = waits.fill.as_secs();
            let ready = now_s >= after_s.saturating_add(wait)
                && payout
                    .fill_early_at
                    .is_none_or(|early| now_s >= early.as_secs().get().saturating_add(wait));
            match payout.intent {
                Some(intent) if ready => Next::ReadFill {
                    chain: payout.terms.dst_chain,
                    intent,
                    after_s,
                },
                _ => Next::Wait,
            }
        }
        // never filled: the user's refund at once
        PayoutStatus::Unfilled => Next::Undo {
            why: UNFILLED.to_string(),
            checked: false,
        },
        // signed: never undone until its transaction is closed
        PayoutStatus::Signed(_)
        | PayoutStatus::Paid
        | PayoutStatus::Undone(_)
        | PayoutStatus::Failed(_) => Next::Wait,
    }
}

/// Serves the queue, first in first out: the head waits once the Base pocket's free USDC
/// covers it, and the next one is looked at only then. Answers how many left the queue.
pub fn serve(now: Timestamp) -> usize {
    let mut served = 0;
    loop {
        let Some((_, head)) = read_state(|state| state.store().payout_queue(1).first().copied())
        else {
            return served;
        };
        let Some(payout) = pool_store::payout(head) else {
            return served;
        };
        if payout.base_hold() > pool_store::ledger().pockets.base.free {
            return served;
        }
        if pool_store::record_at(PoolStep::PayoutWaiting { id: head }, now).is_err() {
            return served;
        }
        served += 1;
    }
}

/// Whether a transaction of payout `id` is being signed or is in the outbox.
fn in_flight(id: u64) -> bool {
    let purpose = TxPurpose::PoolPayout(id);
    let key = purpose
        .quote_hash()
        .expect("BUG: a pool payout's purpose has a key");
    read_state(|state| state.store().has_unsigned_nonce_for(&key))
        || crate::storage::outbox::find(purpose).is_some()
}

/// Why a reverted payout is undone.
pub const REVERTED: &str = "its transaction reverted";

/// Why a reverted payout with no block recorded for its revert stops for a human.
pub const NO_REVERT_BLOCK: &str =
    "ALERT: its transaction reverted in no recorded block, so whether its order ran cannot be read";

/// Whether a waiting planned swap's plan is tried again (the review, 2026-10-23): never
/// signed, tries left, and the plan's own decision before any read ([`plan::decide`]) says it
/// can still fit, or could only revert (the fit then undoes it). A plan that waits whatever
/// the time (not an Order, swapping twice, its payout outside the window, its order deadline
/// too close) is left to the watcher's order.
///
/// [`plan::decide`]: crate::pool::plan::decide
pub fn refittable(payout: &EvmPayout, now: Timestamp) -> bool {
    let (PayoutKind::IcpSwap(terms), Some(plan), Some(amount)) =
        (payout.kind, payout.plan.as_ref(), payout.amount)
    else {
        return false;
    };
    let Some(planned) = terms.planned else {
        return false;
    };
    payout.order.is_none()
        && payout.fit_tries.unwrap_or(0) < types::pool::payout::MAX_FIT_TRIES
        && !matches!(
            crate::pool::plan::decide(plan, planned, amount, now.as_secs().get()),
            Err(crate::pool::plan::Fit::Wait(_))
        )
}

/// Why a payout whose Eco intent never filled is undone.
pub const UNFILLED: &str = "its Eco intent was not filled";

/// The time a delivering payout's Eco intent can last be filled: the one rule for a pinned
/// order's ([`crate::pool::eco::fill_read_after`]); with no order pinned, the latest any route
/// the canister signs may run to past the payout's own deadline.
pub fn fill_read_after(payout: &EvmPayout) -> u64 {
    payout.order.as_ref().map_or_else(
        || {
            payout
                .terms
                .deadline
                .as_secs()
                .get()
                .saturating_add(crate::pool::eco::MAX_ROUTE_LIFE_S)
        },
        crate::pool::eco::fill_read_after,
    )
}

/// The chain reads the pass makes before it undoes a payout that was signed, so a test hands
/// in a fake.
#[allow(async_fn_in_trait)]
pub trait ChainReads {
    /// Whether Swapic on the pool chain says the order ran, read at or after `after`, the
    /// block its transaction reverted in; Err when it cannot be read now.
    async fn ran(&self, order_id: [u8; 32], after: BlockNumber) -> Result<bool, String>;
    /// An Eco intent's fill on `chain` at a block whose time is past `after_s`, and that
    /// block.
    async fn fill_after(
        &self,
        chain: ChainId,
        intent: [u8; 32],
        after_s: u64,
    ) -> Result<(Fill, BlockNumber), crate::agreed::FillRead>;
}

/// The chains themselves, read by every node at an agreed block (M-03).
pub struct LiveReads;

impl ChainReads for LiveReads {
    async fn ran(&self, order_id: [u8; 32], after: BlockNumber) -> Result<bool, String> {
        let chain = crate::storage::config::get()
            .pool_chain
            .ok_or("no pool chain is configured")?;
        crate::agreed::ran(chain, order_id, after).await
    }

    async fn fill_after(
        &self,
        chain: ChainId,
        intent: [u8; 32],
        after_s: u64,
    ) -> Result<(Fill, BlockNumber), crate::agreed::FillRead> {
        crate::agreed::fill_after(chain, intent, after_s).await
    }
}

fn record(step: PoolStep, now: Timestamp) -> bool {
    match pool_store::record_at(step, now) {
        Ok(_) => true,
        Err(error) => {
            log(&format!("a pool payout's line was refused: {error}"));
            false
        }
    }
}

/// Undoes a payout at its deadline: an ICP swap's refund starts (its drive makes it), a
/// withdrawal's shares are minted back.
fn undo(payout: &EvmPayout, why: String, now: Timestamp) -> bool {
    if payout.is_icp_swap() {
        record(PoolStep::PayoutUndoing { id: payout.id, why }, now)
    } else {
        record(
            PoolStep::PayoutUndone {
                id: payout.id,
                why,
                block: None,
            },
            now,
        )
    }
}

/// One pass over the Base payouts, reading the chains themselves: answers how many it acted
/// on.
pub async fn pass(
    ledgers: &(impl Ledgers + PlanReads),
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    pass_with(ledgers, &LiveReads, clock, guard).await
}

/// One pass over the Base payouts, with `reads` for the chain reads: answers how many it
/// acted on.
pub async fn pass_with(
    ledgers: &(impl Ledgers + PlanReads),
    reads: &impl ChainReads,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    let mut acted = serve(clock());
    let Some(settings) = pool_store::ledger().lines().settings else {
        return acted;
    };
    let open: Vec<u64> = pool_store::ledger().lines().open.into_iter().collect();
    // a reverted order's `used` is read on the pool chain, whatever the payout pays on
    let ran = crate::storage::config::get()
        .pool_chain
        .map_or(std::time::Duration::ZERO, crate::agreed::fill_wait_on);
    for id in open {
        let Some(payout) = pool_store::payout(id) else {
            continue;
        };
        let now = clock();
        let Some(_busy) = Busy::take(evm_payout_ref(id).into_bytes()) else {
            continue;
        };
        let waits = Waits {
            fill: crate::agreed::fill_wait_on(payout.terms.dst_chain),
            ran,
        };
        match next(&payout, &settings, now, in_flight(id), waits) {
            Next::Wait => {}
            Next::Drive => match icp_swap::drive(ledgers, id, clock, guard).await {
                Ok(fitted) => {
                    acted += 1;
                    // a planned swap whose amount this drive learnt: its plan, fitted
                    if let Some(fitted) = fitted {
                        crate::pool::plan::sign_fitted(&_busy, id, fitted, clock).await;
                    }
                }
                Err(Stop::Retry(why)) | Err(Stop::Append(why)) => {
                    log(&format!("the pool payout {id} waits: {why}"))
                }
                Err(stop) => log(&format!("the pool payout {id} stopped: {stop:?}")),
            },
            Next::Relist => acted += usize::from(record(PoolStep::PayoutRelisted { id }, now)),
            Next::Fit => match icp_swap::refit(ledgers, id, clock, guard).await {
                Ok(Some(fitted)) => {
                    acted += 1;
                    crate::pool::plan::sign_fitted(&_busy, id, fitted, clock).await;
                }
                Ok(None) => {}
                Err(stop) => log(&format!("the pool payout {id}'s plan: {stop:?}")),
            },
            Next::Stop(why) => {
                log(&format!("the pool payout {id}: {why}"));
                acted += usize::from(record(PoolStep::PayoutStopped { id, why }, now));
            }
            Next::ReadFill {
                chain,
                intent,
                after_s,
            } => match reads.fill_after(chain, intent, after_s).await {
                Ok((Fill::Filled, block)) => {
                    acted += usize::from(record(PoolStep::PayoutDelivered { id, block }, clock()))
                }
                Ok((Fill::Open | Fill::Cancelled, block)) => {
                    if !record(PoolStep::PayoutUnfilled { id, block }, clock()) {
                        continue;
                    }
                    acted += 1;
                    // the user's refund at once; Eco's refund credits the pool later
                    if undo(&payout, UNFILLED.to_string(), clock()) && payout.is_icp_swap() {
                        let _ = icp_swap::drive(ledgers, id, clock, guard).await;
                    }
                }
                // too early: noted, not asked again within one wait
                Err(crate::agreed::FillRead::TooEarly) => {
                    acted += usize::from(record(PoolStep::PayoutFillEarly { id }, clock()))
                }
                // not read: asked again next pass
                Err(crate::agreed::FillRead::Unread(_)) => {}
            },
            Next::Undo { why, checked } => {
                if checked {
                    let Some(pinned) = payout.order.clone() else {
                        continue;
                    };
                    // read at or after the block its transaction reverted in: an older one
                    // says nothing of it (review item 1). A pinned order that never reverted
                    // never reached a block, so any block reads it; a revert in no recorded
                    // block stopped in `next`
                    let after = payout.reverted_block.unwrap_or(BlockNumber::new(0));
                    // a reorg may have turned the revert into a payment: a human's, once
                    match reads.ran(pinned.order_id, after).await {
                        Ok(false) => {}
                        Ok(true) => {
                            let why = "ALERT: its order ran after its revert; it is not undone, for a human".to_string();
                            log(&format!("the pool payout {id}: {why}"));
                            acted +=
                                usize::from(record(PoolStep::PayoutStopped { id, why }, clock()));
                            continue;
                        }
                        Err(_) => continue,
                    }
                }
                let now = clock();
                if undo(&payout, why, now) {
                    acted += 1;
                    // an ICP swap's refund, at once
                    if payout.is_icp_swap() {
                        let _ = icp_swap::drive(ledgers, id, clock, guard).await;
                    }
                }
            }
        }
    }
    acted + serve(clock())
}
