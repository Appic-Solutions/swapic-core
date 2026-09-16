use crate::storage::events;
use crate::storage::halt::set_halted;

/// One pass of the replay audit: the chain must link from genesis and folding the log must
/// reproduce the live state.
pub fn run_replay_audit() {
    record_audit(events::verify_chain() && events::verify_replay());
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
