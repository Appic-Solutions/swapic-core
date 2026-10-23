//! The reads a flow makes of an EVM chain: a simulated call, a gas estimate, and a
//! transfer's receipt at depth. Single unreplicated reads like every other read here
//! (`todo_harden_reads`); each is bound to what the canister itself knows (its address,
//! the configured token, the depth), so a provider can delay a decision but not invent one.

use crate::rpc::{self, hex0x, parse_block_number, parse_hash32, RpcError};
use serde_json::{json, Value};
use types::{BlockDepth, BlockNumber, ChainId, EvmAddress, TokenAmount, TxHash};

/// The most a simulated call's answer may be.
const MAX_CALL_BYTES: u64 = 16_384;

/// What a simulated call answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Simulated {
    /// It returned these bytes.
    Returned(Vec<u8>),
    /// It reverted, with the provider's message.
    Reverted(String),
}

fn call_object(from: EvmAddress, to: EvmAddress, data: &[u8]) -> Value {
    json!({"from": from.to_string(), "to": to.to_string(), "data": hex0x(data)})
}

fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    let text = value.as_str()?.strip_prefix("0x")?;
    hex::decode(text).ok()
}

/// `eth_call` of `data` from `from` to `to` at the latest block.
pub async fn eth_call(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
) -> Result<Simulated, RpcError> {
    call_at(chain_id, from, to, data, json!("latest")).await
}

/// [`eth_call`] whose answer may be up to `max_bytes`: a call that answers more than a word or
/// two, a KyberSwap route rewritten (hex, twice its bytes).
pub async fn eth_call_capped(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
    max_bytes: u64,
) -> Result<Simulated, RpcError> {
    call_capped(chain_id, from, to, data, json!("latest"), max_bytes).await
}

/// `eth_call` of `data` from `from` to `to` at `block`.
pub async fn eth_call_at(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
    block: BlockNumber,
) -> Result<Simulated, RpcError> {
    call_at(
        chain_id,
        from,
        to,
        data,
        json!(format!("0x{:x}", block.get())),
    )
    .await
}

async fn call_at(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
    block: Value,
) -> Result<Simulated, RpcError> {
    call_capped(chain_id, from, to, data, block, MAX_CALL_BYTES).await
}

async fn call_capped(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
    block: Value,
    max_bytes: u64,
) -> Result<Simulated, RpcError> {
    let calls = [("eth_call", json!([call_object(from, to, data), block]))];
    let mut answers = rpc::rpc_batch_each(chain_id, &calls, max_bytes).await?;
    match answers.pop() {
        Some(Ok(value)) => Ok(Simulated::Returned(bytes_of(&value).unwrap_or_default())),
        Some(Err(RpcError::Rpc { message, .. })) => Ok(Simulated::Reverted(message)),
        Some(Err(error)) => Err(error),
        None => Err(RpcError::Unanswered {
            method: "eth_call".to_string(),
        }),
    }
}

/// A simulated call and its gas estimate, in one outcall: the call must return for the
/// estimate to mean anything.
pub async fn simulate_and_estimate(
    chain_id: ChainId,
    from: EvmAddress,
    to: EvmAddress,
    data: &[u8],
) -> Result<(Simulated, Option<u64>), RpcError> {
    let object = call_object(from, to, data);
    let calls = [
        ("eth_call", json!([object.clone(), "latest"])),
        ("eth_estimateGas", json!([object])),
    ];
    let answers = rpc::rpc_batch_each(chain_id, &calls, MAX_CALL_BYTES).await?;
    let mut answers = answers.into_iter();
    let simulated = match answers.next() {
        Some(Ok(value)) => Simulated::Returned(bytes_of(&value).unwrap_or_default()),
        Some(Err(RpcError::Rpc { message, .. })) => Simulated::Reverted(message),
        Some(Err(error)) => return Err(error),
        None => {
            return Err(RpcError::Unanswered {
                method: "eth_call".to_string(),
            })
        }
    };
    let estimate = answers
        .next()
        .and_then(Result::ok)
        .and_then(|value| value.as_str().map(str::to_string))
        .and_then(|text| u64::from_str_radix(text.strip_prefix("0x")?, 16).ok());
    Ok((simulated, estimate))
}

/// What a transfer's receipt showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferRead {
    /// Not mined yet, or not deep enough yet.
    NotYet,
    /// Mined at depth, and it moved `amount` of the token to the address asked about.
    Landed {
        amount: TokenAmount,
        block: BlockNumber,
    },
    /// Mined at depth, and it moved none of the token to that address.
    NoTransfer,
}

/// How much of `token` the transaction `tx_hash` on `chain_id` moved to `to`, once its
/// receipt is `depth` blocks deep: the sum of the token's `Transfer` logs to `to` in a
/// receipt that succeeded. A receipt for another transaction decides nothing.
pub async fn transfer_to(
    chain_id: ChainId,
    tx_hash: TxHash,
    token: EvmAddress,
    to: EvmAddress,
    depth: BlockDepth,
) -> Result<TransferRead, RpcError> {
    let calls = [
        ("eth_blockNumber", json!([])),
        (
            "eth_getTransactionReceipt",
            json!([hex0x(tx_hash.as_ref())]),
        ),
    ];
    let answers = rpc::rpc_batch(chain_id, &calls, 64 * 1024).await?;
    let latest = answers
        .first()
        .and_then(Value::as_str)
        .and_then(parse_block_number);
    let receipt = answers.get(1).filter(|value| !value.is_null());
    let (Some(latest), Some(receipt)) = (latest, receipt) else {
        return Ok(TransferRead::NotYet);
    };
    let Some(block) = receipt
        .get("blockNumber")
        .and_then(Value::as_str)
        .and_then(parse_block_number)
    else {
        return Ok(TransferRead::NotYet);
    };
    if parse_hash32(receipt.get("transactionHash")) != Some(tx_hash.into_bytes()) {
        return Ok(TransferRead::NotYet);
    }
    if !types::tx::is_confirmed(block, latest, depth) {
        return Ok(TransferRead::NotYet);
    }
    if receipt.get("status").and_then(Value::as_str) != Some("0x1") {
        return Ok(TransferRead::NoTransfer);
    }
    let topic = types::router::transfer_topic();
    let mut total = TokenAmount::ZERO;
    for log in receipt
        .get("logs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let from_token = log
            .get("address")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<EvmAddress>().ok().or_else(|| lower(text)));
        if from_token != Some(token) {
            continue;
        }
        let topics: Vec<[u8; 32]> = log
            .get("topics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|topic| parse_hash32(Some(topic)))
            .collect();
        if topics.len() != 3 || topics[0] != topic || topics[2] != to.to_word() {
            continue;
        }
        let Some(value) = log.get("data").and_then(|data| parse_hash32(Some(data))) else {
            continue;
        };
        total = total
            .checked_add(TokenAmount::from_be_bytes(value))
            .unwrap_or(TokenAmount::MAX);
    }
    if total == TokenAmount::ZERO {
        return Ok(TransferRead::NoTransfer);
    }
    Ok(TransferRead::Landed {
        amount: total,
        block,
    })
}

/// An address a provider spelled in lower case, which the checksum parser refuses.
fn lower(text: &str) -> Option<EvmAddress> {
    let bytes = hex::decode(text.strip_prefix("0x")?).ok()?;
    Some(EvmAddress::new(bytes.try_into().ok()?))
}
