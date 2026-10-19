use crate::guards;
use crate::storage::chain_data;
use ic_cdk::update;
pub use settlement_api::types::chain_data::ChainData;
pub use settlement_api::types::errors::PushChainDataError;
use types::{ChainId, Timestamp};

/// Validates `data` and stores it as `chain_id`'s reading, stamped `at`: the canister's own
/// clock, which the caller reads so a unit test runs without a canister. No guard inside:
/// the callers guard first. They are the watcher's push, and the finish and the relay,
/// which carry the reading they were priced on so no separate push has to land before them.
pub fn store_reading(
    chain_id: ChainId,
    data: ChainData,
    at: Timestamp,
) -> Result<(), PushChainDataError> {
    chain_data::put(chain_id, validated(data)?, at);
    Ok(())
}

/// `data` as a reading this canister prices gas with, or the push's refusal of it.
pub fn validated(data: ChainData) -> Result<types::ChainReading, PushChainDataError> {
    types::ChainReading::try_from(data).map_err(|e| PushChainDataError::InvalidData(e.into()))
}

/// Watcher-only. Ambient chain data: the head and the gas prices the watcher saw, which the
/// canister would otherwise spend an outcall on before every decision. It moves no money,
/// so the halt switch does not gate it: a halted canister still wants a current view of the
/// chains while ops investigate.
///
/// The record carries no instant. The canister stamps what it stores with its own clock,
/// because the age of a reading is what every decision on it depends on, and a caller that
/// could date its data forward could keep stale gas prices alive indefinitely.
#[update]
pub fn push_chain_data(chain_id: u64, data: ChainData) -> Result<(), PushChainDataError> {
    guards::require_watcher().map_err(PushChainDataError::Guard)?;
    store_reading(
        ChainId::new(chain_id),
        data,
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
}
