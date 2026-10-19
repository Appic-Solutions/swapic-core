//! The engine: what happens to a flow between the lines its doors and the outbox write.
//!
//! [`drive_all`] runs every open flow's next step (`flows`), and does the calls, reads,
//! sends and appends the step asks for. Every transaction goes through
//! `tx::create_and_send`, so rules A4 to A6 hold here by construction, and every line goes
//! through `append_event`, whose guard is the authority (A2).
//!
//! Event-driven: every line that moves a step arms one coalesced one-shot drive
//! ([`arm`]), so a flow moves the moment its last step completes. The repeating tick is
//! only the backstop, for what no line announces: a withdrawal landing on Ethereum, a
//! balance arriving on a ledger, a read that failed and is worth trying again.
//!
//! The vault-routed swaps (claim, burn, mint and payout through the vaults) are gone: the
//! router and the executor move the money on EVM, and the canister relays, bridges ICP,
//! and refunds what it holds.

pub mod evm_to_icp;
pub mod flows;

use crate::guards::require_not_halted;
use std::cell::Cell;
use types::Timestamp;

/// What one drive did. Returned rather than logged, like the sweep's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drive {
    /// flows the drive acted on
    pub acted: usize,
}

thread_local! {
    // A7: set while a drive is running, cleared when its guard drops, including on a trap.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    // The instant the pending one-shot drive was armed at, while one is pending. Heap by
    // necessity, like every timer: an upgrade clears it and `post_upgrade` arms again.
    static ARMED_AT: Cell<Option<Timestamp>> = const { Cell::new(None) };
    // Set when a drive was asked for while another was running: that drive did not see
    // the line that asked, so one more runs when it ends.
    static AGAIN: Cell<bool> = const { Cell::new(false) };
}

/// Whether a one-shot drive armed at `armed_at` is still pending at `now`. A zero-delay
/// timer fires in a later round, so an arming made at this very instant is still ahead and
/// a second one in the same round coalesces into it. The instant is what decides rather
/// than a flag, for the reason the outbox gives: a callback that cleared a flag and then
/// trapped would leave it set forever, and no drive would ever be armed again.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn still_pending(armed_at: Option<Timestamp>, now: Timestamp) -> bool {
    armed_at.is_some_and(|at| at >= now)
}

/// Asks for one drive as soon as the current message is done. Coalesced: every line that
/// moves a step in the same round asks for the same drive. A drive asked for while another
/// is running runs once that one ends, so no line is ever left to the backstop tick.
pub fn arm() {
    // unit tests run off the canister, where there is no timer to set
    #[cfg(target_arch = "wasm32")]
    {
        let now = Timestamp::from_nanos(ic_cdk::api::time());
        if still_pending(ARMED_AT.get(), now) {
            return;
        }
        ARMED_AT.set(Some(now));
        ic_cdk_timers::set_timer(std::time::Duration::ZERO, || ic_cdk::spawn(drive_now()));
    }
}

/// The one-shot drive [`arm`] schedules, and once more if a line asked for a drive while
/// this one could not run.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
async fn drive_now() {
    if RUNNING.get() {
        AGAIN.set(true);
        return;
    }
    drive_all().await;
    if AGAIN.replace(false) {
        arm();
    }
}

/// Held for the length of one drive, so two drives can never overlap: a drive awaits
/// calls and signatures, and a second one reading the same flows would act twice.
struct DriveGuard;

impl DriveGuard {
    fn take() -> Option<Self> {
        RUNNING.with(|running| {
            if running.get() {
                return None;
            }
            running.set(true);
            Some(Self)
        })
    }
}

impl Drop for DriveGuard {
    fn drop(&mut self) {
        RUNNING.with(|running| running.set(false));
    }
}

/// One drive over the open flows and the pool's payouts. Nothing while halted, and nothing
/// while another drive is still running.
pub async fn drive_all() -> Drive {
    let mut drive = Drive::default();
    let Some(_guard) = DriveGuard::take() else {
        return drive;
    };
    if require_not_halted().is_err() {
        return drive;
    }
    drive.acted = flows::drive_flows().await;
    // the pool's payouts, under the same guard: a drive never overlaps another
    drive.acted += crate::pool::payout::drive(&crate::pool::payout::Live, crate::pool::now).await;
    drive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arming_in_the_same_round_coalesces_and_a_later_one_does_not() {
        let at = Timestamp::from_nanos(1_000);
        assert!(still_pending(Some(at), at));
        assert!(!still_pending(Some(at), Timestamp::from_nanos(1_001)));
        assert!(!still_pending(None, at));
    }
}
