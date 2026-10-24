//! What the unit tests share: one poll of a future no fake leaves waiting, and a ledger's
//! blocks as a fake serves them.

use crate::ic::{CallError, LedgerTransfer};
use crate::pool::resolve::LedgerBlocks;
use candid::Principal;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

/// Runs `future` to its end in one poll: every fake answers at once, so a future still
/// waiting is the test's own bug.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(out) => out,
        std::task::Poll::Pending => panic!("the fake never waits"),
    }
}

/// The ledgers' blocks as the fake answers them, and the reads made.
#[derive(Default)]
pub struct Blocks {
    pub blocks: RefCell<BTreeMap<u64, LedgerTransfer>>,
    pub unreadable: Cell<bool>,
    /// The ledger serves none of the block reads the canister knows.
    pub unserved: Cell<bool>,
    pub reads: RefCell<Vec<(Principal, u64)>>,
}

impl LedgerBlocks for Blocks {
    async fn transfer_at(
        &self,
        ledger: Principal,
        block: u64,
    ) -> Result<Option<LedgerTransfer>, CallError> {
        self.reads.borrow_mut().push((ledger, block));
        if self.unreadable.get() {
            return Err(CallError::Unknown("the ledger did not answer".to_string()));
        }
        if self.unserved.get() {
            return Err(CallError::Unknown(format!(
                "ledger {ledger} answered none of its block reads"
            )));
        }
        Ok(self.blocks.borrow().get(&block).cloned())
    }
}
