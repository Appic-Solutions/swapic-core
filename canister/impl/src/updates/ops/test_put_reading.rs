use crate::guards::require_watcher;
use settlement_api::types::chain_data::ChainData;
use types::{ChainId, Timestamp};

/// Test-only, the watcher's or a controller's: stores `reading` for `chain_id` exactly as a
/// call's reading is stored (`relay::take_reading`), so the integration suite prices a
/// transaction with no outcall for its gas.
#[ic_cdk::update]
pub fn test_put_reading(chain_id: u64, reading: ChainData) -> Result<(), String> {
    require_watcher().map_err(|error| format!("{error:?}"))?;
    crate::relay::take_reading(
        ChainId::new(chain_id),
        Some(reading),
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
}
