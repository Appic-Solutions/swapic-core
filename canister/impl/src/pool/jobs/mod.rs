//! The pool's payouts, each moved by a task of its own (Jeff, 2026-11-10: sales in parallel).
//! A drive picks what can move now, without an await of its own, and starts each payout's next
//! step as a task holding that payout (`Busy`) for as long as it runs: a sale held at one
//! ICPSwap pool holds back no deposit, refund, read or sale anywhere else. At most [`AT_ONCE`]
//! run at once; a drive that finds more work than room leaves the rest, and the next task to
//! end arms one more drive for it.
//!
//! What keeps two tasks apart is what always kept a door's task and the drive apart: one task
//! per payout (`payout::Busy`), one leg per ICPSwap pool (`payout::Leg`, and no leg begins
//! where another is unsettled), and every line checked by the fold in the message that
//! appends it, so two tasks never both take the same pocket's money.

#[cfg(test)]
mod tests;

use std::cell::Cell;

/// The most payout tasks out at once. Each makes calls to ledgers and pools, outcalls and
/// signatures; this keeps a burst of payouts from asking all of them at once.
pub const AT_ONCE: u32 = 10;

thread_local! {
    // Heap by necessity: the tasks out now. An upgrade waits for every outstanding call, so
    // none survives one.
    static OUT: Cell<u32> = const { Cell::new(0) };
    // Set when a drive found work and no room for it: the next task to end arms one more
    // drive.
    static SKIPPED: Cell<bool> = const { Cell::new(false) };
}

/// One task's room among the [`AT_ONCE`], given back on drop, a trap included.
pub struct Slot(());

impl Slot {
    /// Room for one more task, or none: then the drive that asked stops picking, and the next
    /// task to end arms one more drive.
    pub fn take() -> Option<Self> {
        let out = OUT.get();
        if out >= AT_ONCE {
            SKIPPED.set(true);
            return None;
        }
        OUT.set(out + 1);
        Some(Self(()))
    }

    /// How many tasks are out now.
    pub fn out() -> u32 {
        OUT.get()
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OUT.set(OUT.get().saturating_sub(1));
    }
}

/// Whether a drive left work for want of room, cleared by asking: a task that ends and finds it
/// set arms the drive that picks that work up.
pub fn take_skipped() -> bool {
    SKIPPED.replace(false)
}

/// Starts `work` as a task of its own, holding its `slot` until it ends; `work` holds its payout
/// (`Busy`) itself. A task that ends after a drive left work for want of room arms one more
/// drive. Off the canister nothing starts (unit tests run each picked step themselves).
pub fn start<F>(slot: Slot, work: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
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
}
