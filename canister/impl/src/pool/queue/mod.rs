//! The Base payouts' queue, their timeouts and their undo, one pass after every pool
//! movement (the engine's drive) and on every backstop tick while one is open:
//!
//! - every ICP swap still on its way is driven: a pull whose answer was lost asked again (the
//!   same pull, within the ledger's deduplication window), its legs, an undo's refund;
//! - the queue is served first in first out: its head waits the moment the Base pocket's free
//!   USDC covers it, and nothing behind it moves ahead of it;
//! - a waiting payout that waited `order_wait` with no order is listed again, at the back of
//!   `payouts_waiting`;
//! - at its deadline a payout not signed is undone: an ICP swap refunds what it pulled on ICP,
//!   a withdrawal mints its shares back;
//! - a payout is signed once: one whose transaction reverted is undone the same
//!   way, at once, but only once its transaction is closed at the crediting depth and Swapic
//!   says its order never ran (`used(id)` false, read by every node, `agreed::ran`), so a
//!   reorg that turned the revert into a payment is never paid twice; an order that ran after
//!   all stops the payout for a human;
//! - a payout whose order published an Eco intent is delivering until the intent's fill is
//!   read, once, past its route deadline (every node at one block whose time is past it,
//!   `agreed::fill_after`): filled, it is paid; never filled, it is undone at once, the
//!   user refunded, while its USDC comes back from Eco later (`PayoutRefunded`, credited to
//!   the Base pocket only);
//! - a payout whose order deposited on Across (BSC design 6.2.7) is delivering until its fill
//!   is read on the destination, once, at an agreed block past its fill deadline: the
//!   SpokePool's own relay hash must be the canister's (D8), then filled, it is paid; open,
//!   the destination's logs are read for a fill of it (attack M5), and with none it is undone
//!   at once, the user refunded, its USDC owed back by Across (`AcrossRefunded`). A hash that
//!   differs, or a fill logged for a deposit read open, stops it for a human.

#[cfg(test)]
mod tests;

use crate::pool::ck::{self, CkCalls};
use crate::pool::payout::{Busy, Ledgers, Stop, DEDUP_WINDOW};
use crate::pool::plan::{Decided, Fit, PlanReads};
use crate::pool::{icp_swap, log};
use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::pool as pool_store;
use crate::agreed::{AcrossFill, Header};
use types::eco::Fill;
use types::pool::payout::AcrossOut;
use types::events::TxPurpose;
use types::pool::payout::{EvmPayout, PayoutSettings, PayoutStatus};
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
    /// A swap through the ck minter on its way: its own drive (`ck::drive`).
    Ck,
    /// Listed again: it waited `order_wait` with no order.
    Relist,
    /// A planned swap's plan tried again: an earlier try waited. The plan's decision rides
    /// with it, made once ([`refittable`]).
    Fit(Box<Result<Decided, Fit>>),
    /// Undone; `checked`: it was signed once, so only once its transaction is closed for good
    /// and Swapic says its order never ran.
    Undo { why: String, checked: bool },
    /// Its Eco intent's fill read on `chain`, at a block whose time is past `after_s`.
    ReadFill {
        chain: ChainId,
        intent: [u8; 32],
        after_s: u64,
    },
    /// Its Across deposit's fill read on `chain`, at a block whose time is past `after_s`.
    ReadAcrossFill { chain: ChainId, after_s: u64 },
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
    match decide(payout, settings, now, in_flight, waits) {
        Next::Drive if payout.ck.is_some() => Next::Ck,
        decided => decided,
    }
}

fn decide(
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
        // a waiting payout has no order pinned (`order` is set only by `PayoutSigned`): nothing
        // of it can have run
        PayoutStatus::Waiting if late => Next::Undo {
            why: "past its deadline".to_string(),
            checked: false,
        },
        PayoutStatus::Waiting => {
            if let Some(decided) = refittable(payout, now) {
                return Next::Fit(Box::new(decided));
            }
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
        // (once, not a log line every pass)
        PayoutStatus::Reverted if payout.reverted_block.is_none() => {
            Next::Stop(NO_REVERT_BLOCK.to_string())
        }
        // whether its order ran is read once an agreed block can be at or past its revert
        // (not asked every pass before that)
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
            match (payout.intent, &payout.across) {
                (Some(intent), _) if ready => Next::ReadFill {
                    chain: payout.terms.dst_chain,
                    intent,
                    after_s,
                },
                (None, Some(across)) if ready => Next::ReadAcrossFill {
                    chain: across.destination,
                    after_s,
                },
                _ => Next::Wait,
            }
        }
        // an Across deposit never filled: the user waits for Across's refund of it, never
        // fronted by the pool (Jeff, 2026-11-09); its credit begins the undo, and a payout that
        // is no ICP swap is undone here once it is credited
        PayoutStatus::Unfilled => match &payout.across {
            Some(across) if payout.reclaim == Some(types::pool::Reclaim::Credited) => Next::Undo {
                why: across.unfilled_why(),
                checked: false,
            },
            Some(_) => Next::Wait,
            // an Eco intent never filled: the user's refund at once
            None => Next::Undo {
                why: UNFILLED.to_string(),
                checked: false,
            },
        },
        // past its withdrawal the deadline undoes nothing: its drive decides on the minter's
        // word and its own reads
        PayoutStatus::CkWithdrawing | PayoutStatus::CkWithdrawn => Next::Ck,
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
    // the free USDC read once: no await below, so only each `PayoutWaiting` moves it, by
    // the hold it takes
    let mut free = pool_store::ledger().pockets.base.free;
    loop {
        let Some((_, head)) = read_state(|state| state.store().payout_queue(1).first().copied())
        else {
            return served;
        };
        let Some(payout) = pool_store::payout(head) else {
            return served;
        };
        let hold = payout.base_hold();
        if hold > free {
            return served;
        }
        if pool_store::record_at(PoolStep::PayoutWaiting { id: head }, now).is_err() {
            return served;
        }
        free = free.checked_sub(hold).unwrap_or(types::TokenAmount::ZERO);
        served += 1;
    }
}

/// Whether a transaction of payout `id` is being signed or is in the outbox.
fn in_flight(id: u64) -> bool {
    crate::tx::in_flight(TxPurpose::PoolPayout(id))
}

/// Why a reverted payout is undone.
pub const REVERTED: &str = "its transaction reverted";

/// Why a reverted payout with no block recorded for its revert stops for a human.
pub const NO_REVERT_BLOCK: &str =
    "ALERT: its transaction reverted in no recorded block, so whether its order ran cannot be read";

/// The plan's decision when a waiting planned swap's plan is tried again, made once for the
/// try (`refit` takes it): tries left (a waiting payout was
/// never signed), and the plan's own decision before any read ([`plan::decide`]) says it can
/// still fit, or could only revert (the fit then undoes it). None for a plan that waits
/// whatever the time (not an Order, swapping twice, its payout outside the window, its order
/// deadline too close): it is left to the watcher's order.
///
/// [`plan::decide`]: crate::pool::plan::decide
pub fn refittable(payout: &EvmPayout, now: Timestamp) -> Option<Result<Decided, Fit>> {
    if payout.fit_tries.unwrap_or(0) >= types::pool::payout::MAX_FIT_TRIES {
        return None;
    }
    crate::pool::plan::decide_payout(payout, now.as_secs().get())
        .filter(|decided| matches!(decided, Ok(_) | Err(Fit::Undo(_))))
}

/// Why a payout whose Eco intent never filled is undone.
pub const UNFILLED: &str = "its Eco intent was not filled";

/// Why a delivering Across payout stops when the destination's relay hash is not the
/// canister's (D8).
pub const ACROSS_HASH_DIFFERS: &str = "ALERT: the destination SpokePool's relay hash of its Across \
     deposit is not the canister's: whether it was filled is not known, for a human";

/// Why a delivering Across payout read open stops when the destination logged a fill of it
/// (attack M5).
pub const ACROSS_FILL_LOGGED: &str =
    "ALERT: its Across deposit reads open, and the destination logged a fill of it: for a human";

/// Why a delivering Across payout stops when its fill is read too long after its deposit to
/// look for every fill of it in one read.
pub const ACROSS_TOO_LATE: &str = "ALERT: its Across deposit's fill is read too long after the \
     deposit to look for every fill of it: for a human";

/// The time a delivering payout's Eco intent can last be filled: the one rule for a pinned
/// order's ([`crate::pool::eco::fill_read_after`]); with no order pinned, the latest any route
/// the canister signs may run to past the payout's own deadline. An Across deposit's: its fill
/// deadline.
pub fn fill_read_after(payout: &EvmPayout) -> u64 {
    if let Some(across) = &payout.across {
        return across.fill_deadline();
    }
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
    /// An Across deposit's fill on its destination `chain` at a block whose time is past
    /// `after_s`, and that block's header.
    async fn across_fill_after(
        &self,
        chain: ChainId,
        across: &AcrossOut,
        after_s: u64,
    ) -> Result<(AcrossFill, Header), crate::agreed::FillRead>;
    /// Whether the destination `chain` logged a fill of the deposit in the blocks it could be
    /// in up to `header`: Ok(None) when those are too many to read at once.
    async fn fill_logged(
        &self,
        chain: ChainId,
        across: &AcrossOut,
        header: Header,
    ) -> Result<Option<bool>, String>;
    /// The canister's own read of Base, soon: an Across refund it keeps aside for a payout
    /// just read unfilled is credited then, the payout's undo with it. Nothing in a test.
    fn read_base_soon(&self) {}
}

/// The chains themselves, read by every node at an agreed block.
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

    async fn across_fill_after(
        &self,
        chain: ChainId,
        across: &AcrossOut,
        after_s: u64,
    ) -> Result<(AcrossFill, Header), crate::agreed::FillRead> {
        let spoke_pool = spoke_pool_of(chain).map_err(crate::agreed::FillRead::Unread)?;
        crate::agreed::across_fill_after(chain, spoke_pool, &across.relay, after_s).await
    }

    async fn fill_logged(
        &self,
        chain: ChainId,
        across: &AcrossOut,
        header: Header,
    ) -> Result<Option<bool>, String> {
        let spoke_pool = spoke_pool_of(chain)?;
        let block_ms = crate::storage::config::with(|config| {
            config
                .evm_chains
                .get(chain)
                .map(|evm| u64::try_from(evm.block_time.as_millis()).unwrap_or(u64::MAX))
        })
        .unwrap_or(chain.block_seconds().saturating_mul(1_000));
        let Some(pages) = crate::pool::across::log_pages(
            header.number,
            header.timestamp,
            across.deposit_time,
            block_ms,
        ) else {
            return Ok(None);
        };
        crate::agreed::filled_relay_logged(chain, spoke_pool, &across.relay, &pages)
            .await
            .map(Some)
    }

    fn read_base_soon(&self) {
        crate::task_manager::read_base_again();
    }
}

/// `chain`'s Across SpokePool, as the canister's config pins it.
fn spoke_pool_of(chain: ChainId) -> Result<types::EvmAddress, String> {
    crate::storage::config::with(|config| {
        config
            .evm_chains
            .get(chain)
            .and_then(|evm| evm.across)
            .map(|pins| pins.spoke_pool)
    })
    .ok_or_else(|| format!("no Across pins are configured on chain {chain}"))
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

/// One pass over the Base payouts, reading the chains themselves, each picked step run in turn:
/// answers how many it acted on.
#[cfg(test)]
pub async fn pass(
    ledgers: &(impl Ledgers + PlanReads + CkCalls),
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    pass_with(ledgers, &LiveReads, clock, guard).await
}

/// One pass over the Base payouts with `reads` for the chain reads, each picked step run in
/// turn once the pick is over: what a drive does, with the steps one after another instead of
/// as tasks of their own (a task's first part runs as it starts, inside the pick). As in a
/// drive, the queue is served at the pick's start only: what a step frees is served by the
/// next pass, which the step's own line asks for.
#[cfg(test)]
pub async fn pass_with(
    ledgers: &(impl Ledgers + PlanReads + CkCalls),
    reads: &impl ChainReads,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    let mut jobs = Vec::new();
    let picked = pick(clock, &mut |job| {
        jobs.push(job);
        true
    });
    // what the pick did itself, then what each step it handed on did
    let mut acted = picked.saturating_sub(jobs.len());
    for job in jobs {
        acted += run(ledgers, reads, job, clock, guard).await;
    }
    acted
}

/// One Base payout's next step, as a pass picks it, holding the payout while it runs.
pub struct Job {
    pub busy: Busy,
    pub payout: EvmPayout,
    pub next: Next,
}

/// Picks every Base payout that can move now, in turn, after serving the queue: what needs no
/// call is done here (a relist, a stop for a human), and every other step is handed with its
/// hold to `start`, which answers false when there is no room: the pick stops there, and what
/// it left is picked up by the drive the next ending task arms. No await: the steps' calls and
/// reads are the tasks'. Answers how many it acted on: what it did itself and the steps it handed
/// on.
pub fn pick(clock: impl Fn() -> Timestamp + Copy, start: &mut impl FnMut(Job) -> bool) -> usize {
    let mut acted = serve(clock());
    // the ledger read once per pick, not once per payout
    let lines = pool_store::ledger().lines();
    let Some(settings) = lines.settings else {
        return acted;
    };
    for id in lines.open {
        let Some(payout) = pool_store::payout(id) else {
            continue;
        };
        let now = clock();
        let Some(busy) = Busy::take(evm_payout_ref(id).into_bytes()) else {
            continue;
        };
        // what `next` reads only for some statuses is read only for them: the chains' waits
        // for a delivering or reverted payout, a transaction in flight for a waiting or
        // reverted one
        let waits = match payout.status {
            PayoutStatus::Delivering | PayoutStatus::Reverted => {
                let config = crate::storage::config::get();
                // an Across deposit's fill is read on its own destination
                let destination = payout
                    .across
                    .as_ref()
                    .map_or(payout.terms.dst_chain, |across| across.destination);
                Waits {
                    fill: crate::agreed::fill_wait_in(&config, destination),
                    // a reverted order's `used` is read on the pool chain, whatever the
                    // payout pays on
                    ran: config
                        .pool_chain
                        .map_or(std::time::Duration::ZERO, |chain| {
                            crate::agreed::fill_wait_in(&config, chain)
                        }),
                }
            }
            _ => Waits::default(),
        };
        let in_flight = matches!(
            payout.status,
            PayoutStatus::Waiting | PayoutStatus::Reverted
        ) && in_flight(id);
        match next(&payout, &settings, now, in_flight, waits) {
            Next::Wait => {}
            Next::Relist => acted += usize::from(record(PoolStep::PayoutRelisted { id }, now)),
            Next::Stop(why) => {
                log(&format!("the pool payout {id}: {why}"));
                acted += usize::from(record(PoolStep::PayoutStopped { id, why }, now));
            }
            next => {
                if !start(Job { busy, payout, next }) {
                    return acted;
                }
                acted += 1;
            }
        }
    }
    acted
}

/// Runs one picked step, the payout held for it: answers how many it acted on.
pub async fn run(
    ledgers: &(impl Ledgers + PlanReads + CkCalls),
    reads: &impl ChainReads,
    job: Job,
    clock: impl Fn() -> Timestamp + Copy,
    guard: &impl Fn() -> Result<(), String>,
) -> usize {
    let Job {
        busy,
        payout,
        next,
    } = job;
    let id = payout.id;
    let mut acted = 0;
    match next {
        // the pick does these itself and hands none of them on
        Next::Wait | Next::Relist | Next::Stop(_) => {}
        Next::Drive => match icp_swap::drive(ledgers, id, clock, guard).await {
            Ok(fitted) => {
                acted += 1;
                // a planned swap whose amount this drive learnt: its plan, fitted
                if let Some(fitted) = fitted {
                    crate::pool::plan::sign_fitted(&busy, id, fitted, clock).await;
                }
            }
            Err(Stop::Retry(why)) | Err(Stop::Append(why)) => {
                log(&format!("the pool payout {id} waits: {why}"))
            }
            Err(stop) => log(&format!("the pool payout {id} stopped: {stop:?}")),
        },
        Next::Ck => match ck::drive(ledgers, id, clock, guard).await {
            Ok(()) => acted += 1,
            Err(stop) => log(&format!("the ck payout {id} waits: {stop:?}")),
        },
        Next::Fit(decided) => {
            match icp_swap::refit(ledgers, id, *decided, clock, guard).await {
                Ok(Some(fitted)) => {
                    acted += 1;
                    crate::pool::plan::sign_fitted(&busy, id, fitted, clock).await;
                }
                Ok(None) => {}
                Err(stop) => log(&format!("the pool payout {id}'s plan: {stop:?}")),
            }
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
                    return acted;
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
        Next::ReadAcrossFill { chain, after_s } => {
            let Some(across) = payout.across.as_ref() else {
                return acted;
            };
            let stop = |why: &str| {
                log(&format!("the pool payout {id}: {why}"));
                record(
                    PoolStep::PayoutStopped {
                        id,
                        why: why.to_string(),
                    },
                    clock(),
                )
            };
            match reads.across_fill_after(chain, across, after_s).await {
                Ok((AcrossFill::Filled, header)) => {
                    acted += usize::from(record(
                        PoolStep::PayoutDelivered {
                            id,
                            block: header.number,
                        },
                        clock(),
                    ))
                }
                Ok((AcrossFill::HashDiffers, _)) => {
                    acted += usize::from(stop(ACROSS_HASH_DIFFERS))
                }
                // read open: a fill the destination logged stops it, none undoes it
                Ok((AcrossFill::Open, header)) => {
                    match reads.fill_logged(chain, across, header).await {
                        Ok(Some(false)) => {
                            if !record(
                                PoolStep::PayoutUnfilled {
                                    id,
                                    block: header.number,
                                },
                                clock(),
                            ) {
                                return acted;
                            }
                            acted += 1;
                            // no undo now: the user's ckUSDC goes back once Across's refund
                            // is credited, which begins the undo in the same step; a refund
                            // that came early, kept aside, is read again at once
                            if crate::storage::timer_state::get()
                                .across_refunds_waiting
                                .is_some_and(|kept| !kept.is_empty())
                            {
                                reads.read_base_soon();
                            }
                        }
                        Ok(Some(true)) => acted += usize::from(stop(ACROSS_FILL_LOGGED)),
                        Ok(None) => acted += usize::from(stop(ACROSS_TOO_LATE)),
                        // not read: asked again next pass
                        Err(why) => log(&format!(
                            "the pool payout {id}'s Across fill logs were not read: {why}"
                        )),
                    }
                }
                Err(crate::agreed::FillRead::TooEarly) => {
                    acted += usize::from(record(PoolStep::PayoutFillEarly { id }, clock()))
                }
                Err(crate::agreed::FillRead::Unread(_)) => {}
            }
        }
        Next::Undo { why, checked } => {
            if checked {
                let Some(pinned) = payout.order.clone() else {
                    return acted;
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
                        return acted;
                    }
                    Err(_) => return acted,
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
    acted
}
