use crate::storage::halt;
use ic_cdk::query;

/// True once the canister has stopped on a divergence. Three things set it today: the audit
/// timer finding the fold out of step with the log, that same timer finding a bad link in
/// the chunk of the chain it verified, and a controller calling `set_halted`.
///
/// The deep comparison of the fold against a replay of the log is not on that timer: it is
/// the controller-only `audit_replay`, whose cost grows with the log, and it halts the
/// canister on what it finds. The flag also gates the expiry sweep, which appends nothing
/// while it is set; the money endpoints that honour it arrive with the chain layer.
///
/// Nothing clears it but a controller, once the divergence is understood.
#[query]
pub fn halted() -> bool {
    halt::is_halted()
}
