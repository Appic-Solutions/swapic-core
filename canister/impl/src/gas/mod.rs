//! The canister's own gas reading of a chain, for every transaction no call brings a
//! reading with: refunds, approvals, moves, sweeps, cancels and fee bumps. One provider's
//! `eth_feeHistory(5, "latest", [50])`, the call the watcher prices with, cached as any
//! reading is (`storage::chain_data`). One node is enough: whatever it answers is held to
//! the same bounds as a call's reading before anything is signed (`tx::fees`, the per-chain
//! ceiling, the one-ether bound, the action's gas budget).

#[cfg(test)]
mod tests;

use crate::rpc;
use crate::storage::chain_data;
use serde_json::{json, Value};
use types::books::ReadFailure;
use types::{BlockNumber, ChainId, ChainReading, Timestamp, WeiPerGas};

/// The blocks a reading looks back over.
const WINDOW: u64 = 5;

/// The most a five-block `eth_feeHistory` answer may be.
const MAX_FEE_HISTORY_BYTES: u64 = 2_048;

/// One provider's reading of `chain_id` now.
pub async fn price(chain_id: ChainId) -> Result<ChainReading, ReadFailure> {
    let answers = rpc::rpc_batch(
        chain_id,
        &[("eth_feeHistory", json!([WINDOW, "latest", [50]]))],
        MAX_FEE_HISTORY_BYTES,
    )
    .await
    .map_err(|error| error.kind())?;
    answers
        .first()
        .and_then(reading_of)
        .ok_or(ReadFailure::BadAnswer)
}

/// Makes sure `chain_id` has a reading young enough to price a transaction: the cached one,
/// or one read now and cached at the instant it came back. A path that starts a transaction
/// by itself calls it before its own checks, so the pricing in `tx::create_and_send` reads
/// the cache with no await before its nonce is allocated.
pub async fn ensure_fresh(chain_id: ChainId) -> Result<(), ReadFailure> {
    let now = Timestamp::from_nanos(ic_cdk::api::time());
    if chain_data::fresh(chain_id, now).is_some() {
        return Ok(());
    }
    let reading = price(chain_id).await?;
    chain_data::put(
        chain_id,
        reading,
        Timestamp::from_nanos(ic_cdk::api::time()),
    );
    Ok(())
}

/// The reading one `eth_feeHistory` answer gives: its newest block (`oldestBlock` plus the
/// window less one), that block's own base fee (the answer's last base fee is the next
/// block's), and the median of the window's 50th-percentile tips, zero when the chain
/// answers none. None for an answer that does not read.
pub fn reading_of(answer: &Value) -> Option<ChainReading> {
    let quantity = |value: &Value| -> Option<u128> {
        let digits = value.as_str()?.strip_prefix("0x")?;
        (!digits.is_empty())
            .then(|| u128::from_str_radix(digits, 16).ok())
            .flatten()
    };
    let count = answer
        .get("gasUsedRatio")?
        .as_array()?
        .len()
        .checked_sub(1)?;
    let oldest = u64::try_from(quantity(answer.get("oldestBlock")?)?).ok()?;
    let newest = oldest.checked_add(u64::try_from(count).ok()?)?;
    let base_fee = quantity(answer.get("baseFeePerGas")?.get(count)?)?;
    let mut tips = match answer.get("reward") {
        None | Some(Value::Null) => Vec::new(),
        Some(rewards) => rewards
            .as_array()?
            .iter()
            .map(|block| block.get(0).and_then(quantity))
            .collect::<Option<Vec<u128>>>()?,
    };
    tips.sort_unstable();
    let tip = tips
        .get(tips.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or(0);
    Some(ChainReading {
        block: BlockNumber::new(newest),
        base_fee: WeiPerGas::new(base_fee),
        priority_fee: WeiPerGas::new(tip),
    })
}
