use crate::guards::require_watcher_or_controller;
use crate::storage::sanctions::{self, SanctionsError};
use ic_cdk::update;
pub use settlement_api::types::errors::SetSanctionedError;
use types::Address;

/// The watcher's, or a controller's. Adds `add` to the sanctions set and then removes
/// `remove` from it, and answers how many addresses the set holds after. An EVM address is
/// held by its bytes, so any spelling of it is the same entry; other text is held exactly.
/// Compliance data, not money, so the halt switch does not gate it: an operator halting on
/// a divergence still wants the list current.
///
/// The set is bounded, and a call that would take it past the bound is refused whole, with
/// nothing of it written. Every text is held to the 256-byte cap and a longer one is refused
/// by its position, also with nothing written.
#[update]
pub fn set_sanctioned(add: Vec<String>, remove: Vec<String>) -> Result<u64, SetSanctionedError> {
    require_watcher_or_controller().map_err(SetSanctionedError::Guard)?;
    let add = parse_all("add", add)?;
    let remove = parse_all("remove", remove)?;
    sanctions::apply(&add, &remove).map_err(|error| match error {
        SanctionsError::SetFull { capacity } => SetSanctionedError::SetFull { capacity },
    })
}

/// Every text of `list` as an address, or the first that is too long, by its position.
fn parse_all(list: &'static str, texts: Vec<String>) -> Result<Vec<Address>, SetSanctionedError> {
    texts
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            text.parse().map_err(|types::address::TextTooLong { len }| {
                SetSanctionedError::TextTooLong {
                    list: list.to_string(),
                    index: index as u64,
                    len: len as u64,
                }
            })
        })
        .collect()
}
