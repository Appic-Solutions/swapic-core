//! ICRC-10: the standards this canister implements.

use ic_cdk::query;
pub use settlement_api::types::icrc21::SupportedStandard;

/// ICRC-10: the standards this canister implements, ICRC-10 itself and ICRC-21.
#[query]
pub fn icrc10_supported_standards() -> Vec<SupportedStandard> {
    crate::consent::supported_standards()
}
