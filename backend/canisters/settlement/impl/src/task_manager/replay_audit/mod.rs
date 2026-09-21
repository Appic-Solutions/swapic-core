use crate::storage::audit_cursor::{self, AuditCursor};
use crate::storage::events::{self, ReplayWindow};
use crate::storage::halt::set_halted;
use crate::storage::{config, events::ChainChunk};

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

/// What one paged deep check found, and whether it halted the canister.
#[derive(Clone, Debug, PartialEq)]
pub struct AuditReplay {
    pub window: ReplayWindow,
    pub halted: bool,
}

/// The deep check, by hand: folds a window of the log and, where the window is the whole
/// log, compares it with the live fold. A genesis-anchored window that the fold refuses, or
/// that reproduces another state, is a divergence and halts the canister. A window that
/// starts later is a report and nothing more: it cannot be compared, so it cannot condemn.
///
/// Off the timer on purpose: this is the check whose cost grows with the log, and an ops
/// call that runs out of instructions fails that call alone.
pub fn run_audit_replay(start: u64, len: u64) -> AuditReplay {
    let window = events::replay_window(start, len);
    let anchored = window.start == 0;
    let diverged = anchored && (window.refused.is_some() || (window.compared && !window.matches));
    record_audit(!diverged);
    AuditReplay {
        window,
        halted: diverged,
    }
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
