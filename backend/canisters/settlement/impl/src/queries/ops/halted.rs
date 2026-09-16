use crate::storage::halt;
use ic_cdk::query;

/// True once a replay audit has found the log and the live state disagree. Nothing clears
/// it but a controller, and money endpoints refuse while it is set.
#[query]
pub fn halted() -> bool {
    halt::is_halted()
}
