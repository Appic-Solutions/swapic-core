//! The Base payouts' queue, their timeouts and their undo (P11b Task 6), one pass after every
//! pool movement (the engine's drive) and every minute (the payout timer):
//!
//! - every ICP swap still on its way is driven: a pull whose answer was lost asked again (the
//!   same pull, within the ledger's deduplication window), its legs, an undo's refund;
//! - the queue is served first in first out: its head waits the moment the Base pocket's free
//!   USDC covers it, and nothing behind it moves ahead of it;
//! - a waiting payout signed once and reverted is signed again with the order it pinned (the
//!   same id, so a reorg that brings back the first can never pay twice), while that order's
//!   deadline is ahead; one that waited `order_wait` with no order is listed again, at the
//!   back of `payouts_waiting`;
//! - at its deadline a payout not signed is undone: an ICP swap refunds what it pulled on ICP,
//!   a withdrawal mints its shares back. One that was ever signed is undone only once its
//!   transaction is closed at the crediting depth and Swapic says its order never ran
//!   (`used(id)` false, read by every node, `agreed::ran`): a reorg that turned a revert into
//!   a payment is never paid twice.

#[cfg(test)]
mod tests;

use crate::pool::payout::{Busy, Ledgers, Stop, DEDUP_WINDOW};
use crate::pool::{icp_swap, log};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use types::events::TxPurpose;
use types::pool::payout::{EvmPayout, PayoutSettings, PayoutStatus};
use types::pool::{evm_payout_ref, PoolStep};
use types::Timestamp;

/// What the pass does with one payout now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing now.
    Wait,
    /// An ICP swap on its way: its own drive.
    Drive,
    /// Listed again: it waited `order_wait` with no order.
    Relist,
    /// Signed again with the order it pinned, once Swapic says that order never ran.
    Resign,
    /// Undone at its deadline; `checked`: it was signed once, so only once its transaction
    /// is closed for good and Swapic says its order never ran.
    Undo { why: String, checked: bool },
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
        PayoutStatus::Waiting => match &payout.order {
            // reverted: the same order again while it can still land
            Some(pinned) => {
                let ahead = now
                    .as_secs()
                    .get()
                    .saturating_add(crate::pool::pay::ORDER_MARGIN_S);
                if pinned.deadline_s >= ahead {
                    Next::Resign
                } else {
                    Next::Wait
                }
            }
            None => {
                let since = payout.waiting_since.unwrap_or(payout.opened_at);
                if now.saturating_duration_since(since) >= settings.order_wait {
                    Next::Relist
                } else {
                    Next::Wait
                }
            }
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

/// Whether Swapic on the pool chain says the order `id` ran (M-03: read by every node, at
/// an agreed block); Err when it cannot be read now.
async fn ran(order_id: [u8; 32]) -> Result<bool, String> {
    let chain = crate::storage::config::get()
        .pool_chain
        .ok_or("no pool chain is configured")?;
    crate::agreed::ran(chain, order_id).await
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

/// One pass over the Base payouts: answers how many it acted on.
pub async fn pass(
    ledgers: &impl Ledgers,
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
                Ok(()) => acted += 1,
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
            Next::Resign => {
                let Some(pinned) = payout.order.clone() else {
                    continue;
                };
                // a reorg may have turned the revert into a payment: then it never runs again
                match ran(pinned.order_id).await {
                    Ok(false) => {}
                    Ok(true) => {
                        log(&format!(
                            "ALERT: the pool payout {id}'s reverted order ran after all; it waits for a human"
                        ));
                        continue;
                    }
                    Err(_) => continue,
                }
                let Some(order) = types::swapic::decode_order(&pinned.order) else {
                    continue;
                };
                drop(_busy);
                match crate::pool::pay::sign(id, &order, pinned).await {
                    Ok(_) => acted += 1,
                    Err(error) => log(&format!(
                        "the pool payout {id} was not signed again: {error:?}"
                    )),
                }
            }
            Next::Undo { why, checked } => {
                if checked {
                    let Some(pinned) = payout.order.clone() else {
                        continue;
                    };
                    match ran(pinned.order_id).await {
                        Ok(false) => {}
                        Ok(true) => {
                            log(&format!(
                                "ALERT: the pool payout {id}'s order ran after its revert; it is not undone, for a human"
                            ));
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
