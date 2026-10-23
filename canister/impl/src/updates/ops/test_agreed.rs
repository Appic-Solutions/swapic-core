use crate::agreed::{self, FillRead};
use crate::guards::require_controller;
use types::ChainId;

fn word(name: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| format!("{name} is not 32 bytes"))
}

/// Test-only, controller-only door onto [`agreed::ran`], so the integration suite can answer
/// every node's outcall itself.
#[ic_cdk::update]
pub async fn test_agreed_ran(chain_id: u64, order_id: Vec<u8>) -> Result<bool, String> {
    require_controller().map_err(|error| format!("{error:?}"))?;
    agreed::ran(ChainId::new(chain_id), word("order_id", &order_id)?).await
}

/// Test-only, controller-only door onto [`agreed::fill_after`]: the fill and its block, or
/// `TooEarly`, or why it was not read.
#[ic_cdk::update]
pub async fn test_agreed_fill(
    chain_id: u64,
    intent: Vec<u8>,
    after_s: u64,
) -> Result<(String, u64), String> {
    require_controller().map_err(|error| format!("{error:?}"))?;
    match agreed::fill_after(ChainId::new(chain_id), word("intent", &intent)?, after_s).await {
        Ok((fill, block)) => Ok((format!("{fill:?}"), block.get())),
        Err(FillRead::TooEarly) => Err("TooEarly".to_string()),
        Err(FillRead::Unread(why)) => Err(why),
    }
}
