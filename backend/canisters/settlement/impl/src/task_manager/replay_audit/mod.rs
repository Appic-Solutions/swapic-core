use crate::state::transitions::ReplayError;
use crate::state::State;
use crate::storage::audit_cursor::{self, AuditCursor};
use crate::storage::halt::set_halted;
use crate::storage::replay_cursor::{self, ReplayCursor};
use crate::storage::{config, events, events::ChainChunk};

/// One pass of the audit timer, bounded whatever the log holds: the O(1) invariant that the
/// fold is in step with the log, then one chunk of the chain, resumed from the stable
/// cursor. An unbounded pass would trap past the instruction budget, and a callback that
/// traps never halts, which is the one thing this timer exists to do.
pub fn run_replay_audit() {
    record_audit(audit_pass());
}

/// Whether this pass found the canister sound. A broken link, a bad index or a recomputed
/// hash that does not match stops the pass where it is: the cursor stays on the entry that
/// failed, and the canister halts.
fn audit_pass() -> bool {
    if events::ensure_fold_in_step().is_err() {
        return false;
    }
    // a cursor at or past the head has a whole chain behind it, so the next pass starts over
    let cursor = match audit_cursor::get() {
        cursor if cursor.next_index.get() >= events::event_count() => AuditCursor::GENESIS,
        cursor => cursor,
    };
    let chunk = config::get().audit_chunk_events.get().into();
    match events::verify_chain_chunk(cursor.next_index, cursor.parent_hash, chunk) {
        Err(_) => false,
        Ok(ChainChunk {
            next_index,
            parent_hash,
            reached_head,
            ..
        }) => {
            // reaching the head starts the next pass at genesis, so the whole chain is
            // verified again and again rather than once
            audit_cursor::set(if reached_head {
                AuditCursor::GENESIS
            } else {
                AuditCursor {
                    next_index,
                    parent_hash,
                }
            });
            true
        }
    }
}

/// What one step of the deep check found: how far the fold got, and the verdict once it
/// reached the head.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditProgress {
    /// entries folded from genesis, this step's included
    pub folded_so_far: u64,
    /// entries between the fold and the head, as the log stood when this step read it
    pub remaining: u64,
    /// whether this step reached the head and compared the fold with the live one
    pub finished: bool,
    /// whether the fold is the live fold, which only a finished step says
    pub matches: bool,
    /// why the fold stopped, if it did
    pub refused: Option<ReplayError>,
    /// whether this step halted the canister
    pub halted: bool,
}

/// One step of the deep check, by hand: folds up to `max_events` more of the log onto the
/// fold the step before saved, from genesis when there is none, and once the fold reaches
/// the head compares it with the live one. The fold so far is saved in stable memory
/// between steps, so an audit spans as many messages as the log needs and survives an
/// upgrade in the middle. A refusal or a broken link is a divergence wherever it sits, and
/// so is a fold that reaches the head and differs: either halts the canister. A verdict
/// either way puts the next audit back at genesis.
///
/// Off the timer on purpose: this is the check whose cost grows with the log, and an ops
/// call that runs out of instructions fails that call alone and leaves the saved fold
/// where it was.
pub fn run_audit_replay_step(max_events: u64) -> AuditProgress {
    let mut fold = State::new(replay_cursor::get().fold);
    let outcome = events::replay_next(&mut fold, max_events);
    let folded_so_far = fold.meta().next_event_index.get();
    let remaining = events::event_count().saturating_sub(folded_so_far);
    let unfinished = |refused, halted| AuditProgress {
        folded_so_far,
        remaining,
        finished: false,
        matches: false,
        refused,
        halted,
    };
    match outcome {
        // the log does not fold: a divergence, wherever in the log it sits
        Err(error) => {
            finish(false);
            unfinished(Some(error), true)
        }
        // the fold reached the head, so the two folds are compared: the deep check itself
        Ok(0) => {
            let matches = events::read_state(|live| fold.matches(live));
            finish(matches);
            AuditProgress {
                folded_so_far,
                remaining: 0,
                finished: true,
                matches,
                refused: None,
                halted: !matches,
            }
        }
        // more to fold: the fold so far waits for the next step
        Ok(_) => {
            replay_cursor::set(ReplayCursor {
                fold: fold.into_store(),
            });
            unfinished(None, false)
        }
    }
}

/// Ends an audit on its verdict: the next one starts at genesis, and a divergence halts.
fn finish(ok: bool) {
    replay_cursor::set(ReplayCursor::genesis());
    record_audit(ok);
}

/// One audit's verdict. A pass never clears the flag: only a human does, through
/// `set_halted`, once the divergence is understood.
fn record_audit(ok: bool) {
    if !ok {
        set_halted(true);
    }
}

#[cfg(test)]
mod tests;
