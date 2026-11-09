//! The fees users paid before the canister booked them, caught up once. Every finish from
//! before the upgrade names its source transaction in the log, and its calldata names the source
//! chain, but no line holds the fee: the canister threw that number away. So it walks its own log
//! up to the length it had at the upgrade and reads each of those transactions once, through the
//! read a finish makes and the rules a finish books by (`books::user_fee_step`).
//!
//! Nothing starts it but the upgrade and nothing an outsider sends adds to its work. It ends for
//! good at the end of those lines and costs nothing after, and is removed by a later upgrade. A
//! transaction a live finish books meanwhile is booked once: the fold keeps the key.
//!
//! On the pool chain it never claims more than is there: before each past fee, every node reads the
//! balance, and the fee is booked only while the chain is quiet and that balance holds it beyond
//! every line. A fee it cannot read or claim is passed over with an alert line and counted, and the
//! daily proof shows it.

#[cfg(test)]
mod tests;

use crate::agreed;
use crate::books::{self, expected_at};
use crate::cctp::{self, source, Setup};
use crate::state::{State, Store};
use crate::storage::events::{event_at, event_count, read_state};
use crate::storage::halt::is_halted;
use crate::storage::{config, timer_state};
use std::cell::Cell;
use std::time::Duration;
use types::books::{surplus_of, BookStep};
use types::cctp::BurnMessage;
use types::config::EvmChain;
use types::events::{EventType, TxPurpose};
use types::timer_state::FeeCatchUp;
use types::{ChainId, EvmAddress, TokenAmount, TxHash};

/// The most lines one pass looks at, so a pass stays small whatever the log holds.
const LINES_A_PASS: u64 = 200;

/// How soon the next pass runs while there is work.
const NEXT_PASS: Duration = Duration::from_secs(2);

/// How soon a pass runs again after a read failed, or while the canister is halted.
const AFTER_A_FAILURE: Duration = Duration::from_secs(30);

/// How many passes in a row a transaction may wait (a read failed, a halt, the pool chain
/// busy) before it is passed over: about an hour at [`AFTER_A_FAILURE`].
const TRIES: u32 = 120;

thread_local! {
    // Heap by necessity: whether a pass is out right now. An upgrade clears it with the timers.
    static PASSING: Cell<bool> = const { Cell::new(false) };
}

/// Called by every upgrade: the first one fixes where the walk ends, and any one before the walk
/// is done wires its next pass again, since timers live in the heap.
pub fn start() {
    let job = timer_state::update(|state| match state.fee_catch_up.and_then(again) {
        Some(job) if Some(job) == state.fee_catch_up => (job, false),
        Some(job) => {
            state.fee_catch_up = Some(job);
            (job, true)
        }
        None => {
            let job = FeeCatchUp {
                end: event_count(),
                ..FeeCatchUp::default()
            };
            state.fee_catch_up = Some(job);
            (job, true)
        }
    });
    if !job.is_done() {
        arm(NEXT_PASS);
    }
}

/// The job an upgrade carries on with, pure: as it stands, or, once it is done having passed some
/// over, the same lines walked again from the start, so what it gave up on is read once more.
/// What it booked already costs no read on the second walk.
pub fn again(job: FeeCatchUp) -> Option<FeeCatchUp> {
    if job.is_done() && job.passed_over > 0 {
        return Some(FeeCatchUp {
            next: 0,
            end: job.end,
            tries: 0,
            passed_over: 0,
        });
    }
    Some(job)
}

fn arm(delay: Duration) {
    ic_cdk_timers::set_timer(delay, || ic_cdk::spawn(pass()));
}

/// A past transaction still to read: its chain, that chain's configuration, and its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub chain: ChainId,
    pub source: EvmChain,
    pub tx: TxHash,
}

/// What a log line asks of the walk, pure: a finish's creation names the flow that holds its
/// source transaction, and carries the message that names the source chain. None for any other
/// line, for a finish that never signed, for a source of the canister's own, and for a transaction
/// already booked: none of those spends a read.
pub fn due_of<S: Store>(state: &State<S>, line: &EventType, setup: &Setup) -> Option<Due> {
    let EventType::TxCreated {
        purpose: TxPurpose::Finish(key),
        data,
        ..
    } = line
    else {
        return None;
    };
    let store = state.store();
    let tx = store.flow(key)?.source_tx?;
    if store.flow_by_tx(&tx).is_some() || store.pool_own_source(&tx).is_some() {
        return None;
    }
    let message = BurnMessage::parse(types::swapic::finish_message(data)?).ok()?;
    let (chain, source) = cctp::source_of(&message, setup).ok()?;
    let key = types::books::UserFeeKey { chain, tx };
    (!store.is_user_fee_booked(&key)).then_some(Due { chain, source, tx })
}

/// Whether a past fee of `amount` on the pool chain may be booked, pure: what every node read
/// there at `block` (`observed`) holds beyond the lines as they stand now, this job's own lines
/// included, must still hold it. Asked only while no transaction of the canister is open on the
/// chain, so no USDC on its way out still reads in that balance; the pool's `in_flight` (a
/// bridged payout until its fill is read) has left the address and is not taken off again.
pub fn surplus_holds<S: Store>(
    state: &State<S>,
    config: &types::Config,
    chain: ChainId,
    block: types::BlockNumber,
    observed: TokenAmount,
    amount: TokenAmount,
) -> bool {
    let (expected, _) = expected_at(
        state,
        chain,
        config.pool_chain,
        block,
        observed,
        TokenAmount::ZERO,
    );
    let wanted = amount
        .try_into_u128()
        .and_then(|value| i128::try_from(value).ok())
        .unwrap_or(i128::MAX);
    surplus_of(observed, expected) >= wanted
}

/// What one transaction due came to.
enum Booked {
    /// Its line is in, or it had nothing to book.
    Done,
    /// It is passed over for good, with why.
    PassedOver(String),
    /// Not now: the canister is halted, a read failed, or a transaction of the canister on the
    /// pool chain is still open.
    Later(String),
}

/// Reads a past transaction and books what it paid the canister. On the pool chain every node
/// reads the balance first, and the line goes in only while the chain is quiet and that balance
/// holds it. From the last read to the line there is no await.
async fn book(due: &Due, canister: EvmAddress) -> Booked {
    let receipt = match source::read(due.chain, due.tx).await {
        Ok(receipt) => receipt,
        Err(why) => return Booked::Later(why),
    };
    let config = config::get();
    let pool_chain = config.pool_chain == Some(due.chain);
    let balance = if pool_chain {
        match agreed::balance_of(due.chain, due.source.usdc, canister).await {
            Ok((block, raw)) => Some((block, due.source.usdc_to_usd6(raw))),
            Err(why) => return Booked::Later(why),
        }
    } else {
        None
    };
    if is_halted() {
        return Booked::Later("the canister is halted".to_string());
    }
    let outbox_chains = crate::storage::outbox::chains();
    let decided = read_state(|state| {
        let step = books::user_fee_step(
            state,
            due.chain,
            config.pool_chain,
            &receipt,
            &due.source,
            canister,
        );
        let Ok(step) = step else {
            return Ok(None);
        };
        if let (Some((block, observed)), BookStep::UserFeeEarned { amount, .. }) = (balance, &step)
        {
            if crate::sweep::evm::gas_in_use(state, due.chain, &outbox_chains) {
                return Err(Booked::Later(
                    "a transaction of the canister on the pool chain is still open".to_string(),
                ));
            }
            // waited for, not passed over: an arrival or a payout between two reads moves it
            if !surplus_holds(state, &config, due.chain, block, observed, *amount) {
                return Err(Booked::Later(format!(
                    "its {amount} is above what the balance holds beyond the books"
                )));
            }
        }
        Ok(Some(step))
    });
    match decided {
        Ok(Some(step)) => {
            if let Err(error) = books::record(due.chain, step) {
                return Booked::PassedOver(format!("the books refused its line: {error}"));
            }
            Booked::Done
        }
        Ok(None) => Booked::Done,
        Err(later) => later,
    }
}

/// One pass: to the next transaction due, its read, its line, and the next pass.
async fn pass() {
    if PASSING.with(|passing| passing.replace(true)) {
        return;
    }
    let delay = step().await;
    PASSING.with(|passing| passing.set(false));
    if let Some(delay) = delay {
        arm(delay);
    }
}

/// The pass's work: how soon the next one runs, none when the walk is done.
async fn step() -> Option<Duration> {
    let job = timer_state::get().fee_catch_up?;
    if job.is_done() {
        return None;
    }
    if is_halted() {
        return Some(AFTER_A_FAILURE);
    }
    let Ok(canister) = crate::ecdsa::canister_address().await else {
        return Some(AFTER_A_FAILURE);
    };
    let config = config::get();
    let setup = Setup {
        chain_id: config.pool_chain.unwrap_or(ChainId::BASE),
        evm_chains: &config.evm_chains,
        domains: &config.cctp_domains,
        canister,
    };
    let stop = job.end.min(job.next.saturating_add(LINES_A_PASS));
    let mut at = job.next;
    let mut due = None;
    while at < stop && due.is_none() {
        if let Some(event) = event_at(at) {
            due = read_state(|state| due_of(state, &event.payload, &setup));
        }
        if due.is_none() {
            at += 1;
        }
    }
    let Some(due) = due else {
        return save(FeeCatchUp {
            next: at,
            tries: 0,
            ..job
        });
    };
    // past the awaits the log has moved on, but only ever past the end of this walk's lines
    match book(&due, canister).await {
        Booked::Done => save(FeeCatchUp {
            next: at + 1,
            tries: 0,
            ..job
        }),
        Booked::PassedOver(why) => passed_over(job, at, &due, &why),
        Booked::Later(why) if job.tries + 1 >= TRIES => passed_over(job, at, &due, &why),
        Booked::Later(_) => {
            save(FeeCatchUp {
                next: at,
                tries: job.tries + 1,
                ..job
            });
            Some(AFTER_A_FAILURE)
        }
    }
}

/// A transaction given up on: counted, with an alert line, and the walk goes on past it.
fn passed_over(job: FeeCatchUp, at: u64, due: &Due, why: &str) -> Option<Duration> {
    ic_cdk::println!(
        "ALERT: the past fee of {} on chain {} is passed over: {why}",
        due.tx,
        due.chain
    );
    save(FeeCatchUp {
        next: at + 1,
        tries: 0,
        passed_over: job.passed_over.saturating_add(1),
        ..job
    })
}

/// Keeps where the walk stands; how soon the next pass runs, none when it is done.
fn save(job: FeeCatchUp) -> Option<Duration> {
    timer_state::update(|state| {
        state.fee_catch_up = Some(job);
        ((), true)
    });
    (!job.is_done()).then_some(NEXT_PASS)
}
