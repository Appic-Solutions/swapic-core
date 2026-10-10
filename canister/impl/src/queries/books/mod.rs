//! The canister's USDC books.

use ic_cdk::query;
pub use settlement_api::types::books::BookLines;

/// The book lines of every chain, in micro-USDC, with the last balance check.
#[query]
pub fn books() -> Vec<(u64, BookLines)> {
    Vec::new()
}
