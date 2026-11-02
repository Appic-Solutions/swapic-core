use crate::storage::halt;
use ic_cdk::query;

/// True once a controller stopped the canister with `set_halted`. While it is set nothing
/// is signed, sent or paid. Nothing clears it but a controller.
#[query]
pub fn halted() -> bool {
    halt::is_halted()
}
