//! ICRC-21: the consent message a wallet shows its user before it signs a call.

use crate::consent::{self, Known};
use ic_cdk::update;
pub use settlement_api::types::icrc21::{ConsentMessageRequest, ConsentMessageResponse};

/// ICRC-21: a plain-words description of a call to `swap_from_icp`, `swap_from_icp_ck`, `stake_icp`,
/// `withdraw_icp` or `withdraw_evm`, read from its argument exactly as that door reads it,
/// for a wallet to show its user before it signs; an ICRC-21 error for any other call or an
/// argument the door would not take. Any caller, anonymous included; never halted; writes
/// nothing and awaits nothing. An update, as ICRC-21 declares it.
#[update]
pub fn icrc21_canister_call_consent_message(
    request: ConsentMessageRequest,
) -> ConsentMessageResponse {
    let ckusdc = crate::storage::pool::ledger()
        .settings
        .map(|settings| settings.ckusdc_ledger);
    crate::storage::config::with(|config| {
        consent::answer(
            &request,
            &Known {
                config,
                canister: crate::storage::ecdsa_address::get(),
                ckusdc,
                caller: ic_cdk::api::caller(),
            },
        )
    })
}
