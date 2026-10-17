//! The canister's USDC books.

use crate::books::in_flight;
use crate::state::Store;
use crate::storage::config;
use crate::storage::events::read_state;
use candid::{Int, Nat};
use ic_cdk::query;
pub use settlement_api::types::books::BookLines;
use std::collections::BTreeSet;
use types::ChainId;

/// The book lines of every chain, in micro-USDC, with the last balance check.
#[query]
pub fn books() -> Vec<(u64, BookLines)> {
    let mut chains: BTreeSet<ChainId> = config::get().evm_chains.0.into_keys().collect();
    read_state(|state| {
        chains.extend(state.store().books().into_iter().map(|(chain, _)| chain));
        chains
            .into_iter()
            .map(|chain| {
                let books = state.book(&chain);
                let check = books.last_check;
                let lines = BookLines {
                    fee_income: Nat::from(books.fee_income),
                    unattributed: Nat::from(books.unattributed),
                    in_flight: Nat::from(in_flight(state, chain)),
                    pool: Nat::from(books.pool),
                    observed: check.map(|check| Nat::from(check.observed)),
                    surplus: check.map(|check| Int::from(check.surplus())),
                    checked_at_block: check.map(|check| check.block.get()),
                    checked_at_ns: books.checked_at.map(|at| at.as_nanos()),
                    failed_at_ns: books.failed_at.map(|at| at.as_nanos()),
                };
                (chain.get(), lines)
            })
            .collect()
    })
}
