//! The pool's payouts, each moved by a task of its own (Jeff, 2026-11-10: sales in parallel).
//! A drive picks what can move now, without an await of its own, and starts each payout's next
//! step as a task holding that payout (`Busy`) for as long as it runs: a sale held at one
//! ICPSwap pool holds back no deposit, refund, read or sale anywhere else. At most [`AT_ONCE`]
//! run at once in each [`Lane`], so the payouts on ICP and the Base payouts never take each
//! other's room; a drive that finds more work than room leaves the rest, and the next task to
//! end arms one more drive for it (a task that traps leaves that to the backstop).
//!
//! What keeps two tasks apart is what always kept a door's task and the drive apart: one task
//! per payout (`payout::Busy`), one leg per ICPSwap pool (`payout::Leg`, and no leg begins
//! where another is unsettled), and every line checked by the fold in the message that
//! appends it, so two tasks never both take the same pocket's money.

#[cfg(test)]
mod tests;

use std::cell::Cell;

/// The most payout tasks out at once in one lane. Each makes calls to ledgers and pools,
/// outcalls and signatures; this keeps a burst of payouts from asking all of them at once.
pub const AT_ONCE: u32 = 10;

/// Which room a task takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// What the pool pays on ICP: deposits' payouts, stakes, withdrawals on ICP.
    Icp,
    /// The Base payouts: ICP swaps, EVM withdrawals, their reads and undos.
    Base,
}

thread_local! {
    // Heap by necessity: the tasks out now in each lane. An upgrade waits for every
    // outstanding call, so none survives one.
    static OUT: [Cell<u32>; 2] = const { [Cell::new(0), Cell::new(0)] };
    // Set when a drive found work and no room for it: the next task to end arms one more
    // drive.
    static SKIPPED: Cell<bool> = const { Cell::new(false) };
}

fn index(lane: Lane) -> usize {
    match lane {
        Lane::Icp => 0,
        Lane::Base => 1,
    }
}

/// One task's room among its lane's [`AT_ONCE`], given back on drop, a trap included.
pub struct Slot(Lane);

impl Slot {
    /// Room for one more task in `lane`, or none: then the drive that asked stops picking
    /// there, and the next task to end arms one more drive.
    pub fn take(lane: Lane) -> Option<Self> {
        OUT.with(|out| {
            let out = &out[index(lane)];
            if out.get() >= AT_ONCE {
                SKIPPED.set(true);
                return None;
            }
            out.set(out.get() + 1);
            Some(Self(lane))
        })
    }

    /// How many tasks are out now in `lane`.
    pub fn out(lane: Lane) -> u32 {
        OUT.with(|out| out[index(lane)].get())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OUT.with(|out| {
            let out = &out[index(self.0)];
            out.set(out.get().saturating_sub(1));
        });
    }
}

/// Whether a drive left work for want of room, cleared by asking: a task that ends and finds it
/// set arms the drive that picks that work up.
pub fn take_skipped() -> bool {
    SKIPPED.replace(false)
}

/// Starts `work` as a task of its own in `lane` when there is room (false when there is none),
/// holding the room until it ends; `work` holds its payout (`Busy`) itself. A task that ends
/// after a drive left work for want of room arms one more drive. Off the canister nothing runs
/// (unit tests run each picked step themselves).
pub fn start<F>(lane: Lane, work: F) -> bool
where
    F: std::future::Future<Output = ()> + 'static,
{
    let Some(slot) = Slot::take(lane) else {
        return false;
    };
    #[cfg(target_arch = "wasm32")]
    ic_cdk::spawn(async move {
        work.await;
        drop(slot);
        if take_skipped() {
            crate::engine::arm();
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (slot, work);
    true
}
