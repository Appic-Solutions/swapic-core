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
use types::pool::payout::{EvmPayout, PayoutSettings, PayoutStatus};
use types::pool::{evm_payout_ref, PoolStep};
use types::{BlockNumber, ChainId, Timestamp};

/// How long after an Eco intent's route deadline its fill is read: the agreed block lags the
/// head by the crediting depth and a little more.
pub const FILL_MARGIN_S: u64 = 60;

/// What the pass does with one payout now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing now.
    Wait,
    /// An ICP swap on its way: its own drive.
    Drive,
    /// Listed again: it waited `order_wait` with no order.
    Relist,
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
/// still being signed or is in the outbox.
pub fn next(
    payout: &EvmPayout,
    settings: &PayoutSettings,
    now: Timestamp,
    in_flight: bool,
) -> Next {
    let late = now >= payout.terms.deadline;
    match &payout.status {
        // a pull too old to deduplicate can no longer be asked again safely: a human's
        PayoutStatus::Opened if now.saturating_duration_since(payout.opened_at) >= DEDUP_WINDOW => {
            Next::Stop("ALERT: the pull's outcome can no longer be asked for".to_string())
        }
        PayoutStatus::Opened | PayoutStatus::Swapping | PayoutStatus::Undoing(_) => Next::Drive,
        // the place in the queue is the serve's to change; the deadline undoes it
        PayoutStatus::Known | PayoutStatus::Queued if late => Next::Undo {
            why: "past its deadline".to_string(),
            checked: false,
        },
        PayoutStatus::Known => Next::Drive,
        PayoutStatus::Queued => Next::Wait,
        PayoutStatus::Waiting if in_flight => Next::Wait,
        PayoutStatus::Waiting if late => Next::Undo {
            why: "past its deadline".to_string(),
            checked: payout.order.is_some(),
        },
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
        PayoutStatus::Reverted => Next::Undo {
            why: REVERTED.to_string(),
            checked: true,
        },
        // its Eco intent's fill, read once its route can no longer be filled
        PayoutStatus::Delivering => {
            let after_s = fill_read_after(payout);
            match payout.intent {
                Some(intent) if now.as_secs().get() >= after_s.saturating_add(FILL_MARGIN_S) => {
                    Next::ReadFill {
                        chain: payout.terms.dst_chain,
                        intent,
                        after_s,
                    }
                }
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

/// Why a payout whose Eco intent never filled is undone.
pub const UNFILLED: &str = "its Eco intent was not filled";

/// The time a delivering payout's Eco intent can last be filled: its route's deadline, read
/// from the order it pinned; else the latest any route the canister signs may run to.
pub fn fill_read_after(payout: &EvmPayout) -> u64 {
    let pinned = payout.order.as_ref();
    pinned
        .and_then(|pinned| crate::pool::rebalance::drive::eco_route_deadline(&pinned.order))
        .unwrap_or_else(|| {
            pinned
                .map_or(payout.terms.deadline.as_secs().get(), |pinned| {
                    pinned.deadline_s
                })
                .saturating_add(crate::pool::eco::MAX_ROUTE_LIFE_S)
        })
}

/// The chain reads the pass makes before it undoes a payout that was signed, so a test hands
/// in a fake.
#[allow(async_fn_in_trait)]
pub trait ChainReads {
    /// Whether Swapic on the pool chain says the order ran; Err when it cannot be read now.
    async fn ran(&self, order_id: [u8; 32]) -> Result<bool, String>;
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
    async fn ran(&self, order_id: [u8; 32]) -> Result<bool, String> {
        let chain = crate::storage::config::get()
            .pool_chain
            .ok_or("no pool chain is configured")?;
        crate::agreed::ran(chain, order_id).await
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
    for id in open {
        let Some(payout) = pool_store::payout(id) else {
            continue;
        };
        let now = clock();
        let Some(_busy) = Busy::take(evm_payout_ref(id).into_bytes()) else {
            continue;
        };
        match next(&payout, &settings, now, in_flight(id)) {
            Next::Wait => {}
            Next::Drive => match icp_swap::drive(ledgers, id, clock, guard).await {
                Ok(fitted) => {
                    acted += 1;
                    // a planned swap whose amount this drive learnt: its plan, fitted
                    if let Some(fitted) = fitted {
                        if let Err(error) =
                            crate::pool::pay::sign_held(&_busy, id, &fitted.order, fitted.pinned)
                                .await
                        {
                            log(&format!(
                                "the pool payout {id}'s plan was not signed ({error:?}): it waits for an order"
                            ));
                        }
                    }
                }
                Err(Stop::Retry(why)) | Err(Stop::Append(why)) => {
                    log(&format!("the pool payout {id} waits: {why}"))
                }
                Err(stop) => log(&format!("the pool payout {id} stopped: {stop:?}")),
            },
            Next::Relist => acted += usize::from(record(PoolStep::PayoutRelisted { id }, now)),
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
                // asked again next pass
                Err(_) => {}
            },
            Next::Undo { why, checked } => {
                if checked {
                    let Some(pinned) = payout.order.clone() else {
                        continue;
                    };
                    // a reorg may have turned the revert into a payment: a human's, once
                    match reads.ran(pinned.order_id).await {
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
