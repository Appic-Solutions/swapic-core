use crate::guards;
use crate::storage::chain_data;
use ic_cdk::update;
pub use settlement_api::types::chain_data::ChainData;
pub use settlement_api::types::errors::PushChainDataError;
use types::{ChainId, Timestamp};

/// Watcher-only. Ambient chain data: the head and the gas prices the watcher saw, which the
/// canister would otherwise spend an outcall on before every decision. It moves no money,
/// so the halt switch does not gate it: a halted canister still wants a current view of the
/// chains while an operator investigates.
///
/// The record carries no instant. The canister stamps what it stores with its own clock,
/// because the age of a reading is what every decision on it depends on, and a caller that
/// could date its data forward could keep stale gas prices alive indefinitely.
#[update]
pub fn push_chain_data(chain_id: u64, data: ChainData) -> Result<(), PushChainDataError> {
    guards::require_watcher().map_err(PushChainDataError::Guard)?;
    let reading = types::ChainReading::try_from(data)
        .map_err(|e| PushChainDataError::InvalidData(e.into()))?;
    chain_data::put(
        ChainId::new(chain_id),
        reading,
        Timestamp::from_nanos(ic_cdk::api::time()),
    );
    Ok(())
}
