//! The canister's USDC books.

use crate::state::Store;
use crate::storage::events::read_state;
use crate::storage::{config, timer_state};
use candid::{Int, Nat};
use ic_cdk::query;
pub use settlement_api::types::books::BookLines;
use std::collections::BTreeSet;
use types::ChainId;

/// The book lines of every chain, in micro-USDC, with the last balance read.
#[query]
pub fn books() -> Vec<(u64, BookLines)> {
    let mut chains: BTreeSet<ChainId> = config::get().evm_chains.0.into_keys().collect();
    let reads = timer_state::get().books_reads;
    read_state(|state| {
        chains.extend(state.store().books().into_iter().map(|(chain, _)| chain));
        chains
            .into_iter()
            .map(|chain| {
                let books = state.book(&chain);
                let read = reads.get(&chain).copied().unwrap_or_default();
                // the read kept beside the timers, or the log's when it is as new: a
                // controller's booking of the surplus has moved that one since
                let check = match (books.last_check, read.last()) {
                    (Some(logged), Some(kept)) if logged.block >= kept.block => Some(logged),
                    (logged, kept) => kept.or(logged),
                };
                let lines = BookLines {
                    fee_income: Nat::from(books.fee_income),
                    unattributed: Nat::from(books.unattributed),
                    // the v1 flows' USDC on the chain: none since they stopped, kept on the
                    // wire
                    in_flight: Nat::from(0_u8),
                    pool: Nat::from(books.pool),
                    observed: check.map(|check| Nat::from(check.observed)),
                    surplus: check.map(|check| Int::from(check.surplus())),
                    checked_at_block: check.map(|check| check.block.get()),
                    checked_at_ns: read.read_at().map(|at| at.as_nanos()),
                    failed_at_ns: read.failed_at.map(|at| at.as_nanos()),
                };
                (chain.get(), lines)
            })
            .collect()
    })
}
