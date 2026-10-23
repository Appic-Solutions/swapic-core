//! The cost guard on the arrival reports anyone may make (`note_pool_arrival`,
//! `note_move_arrival`, P11e): each costs the canister a receipt outcall of up to 512 KB, so a
//! caller other than the watcher is held to at most [`PER_MINUTE`] reports a minute and
//! [`PER_HOUR`] an hour in total (each costs a read every node makes, M-03), and a given
//! `base_tx` to at most [`PER_TX_PER_HOUR`] attempts an hour, refused before any outcall. The watcher's principal is never limited. The reports let through are kept in
//! stable memory (C-09, `storage::timer_state`), so an upgrade grants no extra burst.

use crate::storage::{roles, timer_state};
use settlement_api::types::pool::PoolError;
pub use types::timer_state::{Limiter, PER_HOUR, PER_MINUTE, PER_TX_PER_HOUR};
use types::{Timestamp, TxHash};

/// The guard for a report of `base_tx` by the caller at `now`: the watcher always passes.
pub fn admit(base_tx: TxHash, now: Timestamp) -> Result<(), PoolError> {
    if roles::get().watcher == Some(ic_cdk::api::caller()) {
        return Ok(());
    }
    timer_state::update(|state| state.reports.admit(base_tx, now)).map_err(PoolError::Unavailable)
}
