use crate::guards::require_controller;
use crate::storage::config;
use types::{BlockNumber, ChainId, Timestamp, TokenAmount};

/// Test-only, controller-only: keeps a balance read of `chain_id`, `observed` USDC at
/// `block`, exactly as the balance check keeps the one it read (`books::keep_read`), so the
/// integration suite opens the pool's guard with no outcall for the head and the balance.
#[ic_cdk::update]
pub fn test_keep_read(chain_id: u64, block: u64, observed: u64) -> Result<(), String> {
    require_controller().map_err(|error| format!("{error:?}"))?;
    crate::books::keep_read(
        &config::get(),
        ChainId::new(chain_id),
        BlockNumber::new(block),
        TokenAmount::from(observed),
        Timestamp::from_nanos(ic_cdk::api::time()),
    )
    .map_err(|failure| format!("{failure:?}"))
}
