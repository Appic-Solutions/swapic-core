//! The one door onto a chain: JSON-RPC over an unreplicated HTTPS outcall.
//!
//! Every read here is a single-provider read (`todo_harden_reads`: upgrade to k-of-n, or to
//! the flexible outcall mode, later). Unreplicated is what the canister wants for the
//! reasons that hold under either pricing version: one request instead of one per node, so
//! a provider's rate limiter sees one caller, and no transform to agree on. The cycle
//! saving needs [`PRICING_VERSION_PAY_AS_YOU_GO`] as well, because version 1 prices a call
//! by its reserved `max_response_bytes` and ignores the replication mode entirely
//! (measured on mainnet 2026-09-21: 87.44M replicated against 87.56M unreplicated).
//!
//! `ic-cdk 0.17` carries neither field on its outcall argument, so the argument is built
//! here, as the research canister proved live.

use crate::storage::config;
use candid::{CandidType, Principal};
use ic_cdk::api::call::call_with_payment128;
use ic_cdk::api::management_canister::http_request::{
    HttpHeader, HttpMethod, HttpResponse, TransformContext,
};
use serde_json::{json, Value};
use thiserror::Error;
use types::{BlockNumber, ChainId, RpcUrl};

/// Pay-as-you-go pricing: charges what the call consumed rather than what it reserved, and
/// it is the only version that prices an unreplicated call as one.
const PRICING_VERSION_PAY_AS_YOU_GO: u32 = 2;

/// The largest subnet this canister could be placed on. Every reservation is sized for it,
/// so a move to the 34-node fiduciary subnet cannot leave a call short of cycles.
const MAX_SUBNET_NODES: u128 = 34;

/// The round trip the pricing formula caps at, in milliseconds.
const MAX_ROUNDTRIP_MS: u128 = 60_000;

/// What the argument [`post`] builds must always be. Held at the one place the call is
/// made, so no future caller can build the argument by hand and lose the policy.
fn assert_outcall_policy(request: &HttpRequest) {
    assert_eq!(
        (request.is_replicated, request.pricing_version),
        (Some(false), Some(PRICING_VERSION_PAY_AS_YOU_GO)),
        "BUG: every outcall is unreplicated and priced pay-as-you-go"
    );
}

/// The management canister's `http_request` argument, with the two fields `ic-cdk 0.17`
/// does not carry. Candid matches record fields by name, so the callee reads the fields it
/// knows and this one stays a plain record.
#[derive(CandidType, Clone, PartialEq, Eq)]
struct HttpRequest {
    url: String,
    max_response_bytes: Option<u64>,
    method: HttpMethod,
    headers: Vec<HttpHeader>,
    body: Option<Vec<u8>>,
    transform: Option<TransformContext>,
    /// `Some(false)`: one replica makes the request, not all of them.
    is_replicated: Option<bool>,
    /// `opt nat32` on the wire, per interface specification 0.68.0.
    pricing_version: Option<u32>,
}

/// The url is a secret: it carries the provider's api key, and every other type that holds
/// one prints `***`. Written by hand rather than derived so a `{request:?}` a future
/// caller adds cannot be the one place it leaks.
impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &"***")
            .field("max_response_bytes", &self.max_response_bytes)
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field("is_replicated", &self.is_replicated)
            .field("pricing_version", &self.pricing_version)
            .finish()
    }
}

/// The most of a text this canister did not write that an error carries out of this
/// module: enough for an operator to tell one failure from another in a log, and never a
/// provider's whole answer.
pub const MAX_SUMMARY_BYTES: usize = 200;

/// A bounded summary of text that came from outside: the system's reject message, or a
/// provider's body. It can only be made by [`Summary::of`], so no caller can put a
/// provider's megabyte into an error that a quoter or a watcher reads back, and no control
/// character can travel with it into whatever reads the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary(String);

impl Summary {
    /// `text` with its control characters turned into spaces, cut to
    /// [`MAX_SUMMARY_BYTES`] at a character boundary.
    pub fn of(text: &str) -> Self {
        let cleaned: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let mut end = cleaned.len().min(MAX_SUMMARY_BYTES);
        while !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        Self(cleaned[..end].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a read of a chain answered nothing usable.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RpcError {
    #[error("no rpc url is configured for chain {0}")]
    NoUrl(ChainId),
    #[error("the outcall was rejected: {reason}")]
    Unreachable { reason: Summary },
    #[error("the provider answered http {status}: {body}")]
    Http { status: u16, body: Summary },
    #[error("a batch of {calls} is answered by an array, and this is not one: {reason}")]
    NotAnArray { calls: usize, reason: String },
    #[error("{asked} calls were answered by {answered} replies")]
    WrongReplyCount { asked: usize, answered: usize },
    #[error("a reply carries the id {id:?}, which this batch of {calls} never asked for")]
    UnknownReplyId { id: Option<u64>, calls: usize },
    #[error("id {id} was answered twice")]
    DuplicateReplyId { id: usize },
    #[error("{method} was not answered")]
    Unanswered { method: String },
    #[error("{method} answered neither a result nor an error")]
    NeitherResultNorError { method: String },
    #[error("{method} answered an error: {message}")]
    Rpc { method: String, message: String },
}

/// The outcall argument for one POST to `url`. The one place `is_replicated` and
/// `pricing_version` are set.
fn post(url: &RpcUrl, body: Vec<u8>, max_response_bytes: u64) -> HttpRequest {
    HttpRequest {
        url: url.expose().to_string(),
        max_response_bytes: Some(max_response_bytes),
        method: HttpMethod::POST,
        headers: vec![HttpHeader {
            name: "Content-Type".to_string(),
            value: "application/json".to_string(),
        }],
        body: Some(body),
        // an unreplicated call reaches no consensus, so there is nothing to strip
        transform: None,
        is_replicated: Some(false),
        pricing_version: Some(PRICING_VERSION_PAY_AS_YOU_GO),
    }
}

/// The cycles to attach, by the version 2 formula for a non-replicated call, at the worst
/// response the cap admits, the capped round trip, and the largest subnet this canister
/// could sit on, then doubled. Version 2 treats the attachment as the budget the call runs
/// within and refunds what it does not spend, so a generous reservation costs nothing and a
/// tight one shrinks the call's own limits.
fn cycles_for(request_bytes: u64, max_response_bytes: u64) -> u128 {
    let n = MAX_SUBNET_NODES;
    let request = u128::from(request_bytes);
    let response = u128::from(max_response_bytes);
    // min_responses is 1 for a non-replicated call
    let replication = 90_000 * n + (2_000 * n + 100_000);
    let base = (1_000_000 + 50 * request + replication) * n;
    // the per-node usage term is charged for the one node that makes the call, plus the
    // non-replicated surcharge of 50 * n per response byte
    let usage = 50 * response + 300 * MAX_ROUNDTRIP_MS + 50 * n * response;
    let delivery = n * (10 * n + 600) * response;
    2 * (base + usage + delivery)
}

/// One POST, unreplicated, with the cycles the call reserves. The body of a 2xx answer, or
/// why there is none.
async fn http_post(
    url: &RpcUrl,
    body: Vec<u8>,
    max_response_bytes: u64,
) -> Result<Vec<u8>, RpcError> {
    let request = post(url, body, max_response_bytes);
    assert_outcall_policy(&request);
    let request_bytes = request_size(&request);
    let cycles = cycles_for(request_bytes, max_response_bytes);
    let (response,): (HttpResponse,) = call_with_payment128(
        Principal::management_canister(),
        "http_request",
        (request,),
        cycles,
    )
    .await
    .map_err(|(code, message)| RpcError::Unreachable {
        reason: Summary::of(&format!("{code:?}: {message}")),
    })?;
    // a status outside a u16 is no http status at all, and is not a 2xx either
    let status: u16 = response.status.0.try_into().unwrap_or(u16::MAX);
    if !(200..300).contains(&status) {
        return Err(RpcError::Http {
            status,
            body: Summary::of(&String::from_utf8_lossy(&response.body)),
        });
    }
    Ok(response.body)
}

/// What the pricing formula counts as the request: the url, the headers and the body.
fn request_size(request: &HttpRequest) -> u64 {
    let headers: usize = request
        .headers
        .iter()
        .map(|header| header.name.len() + header.value.len())
        .sum();
    let bytes = request.url.len() + headers + request.body.as_ref().map_or(0, Vec::len);
    u64::try_from(bytes).expect("BUG: usize is at most 64 bits on every target")
}

/// The most an `eth_blockNumber` reply may be: a hex height and the JSON-RPC envelope
/// around it.
pub(crate) const MAX_BLOCK_NUMBER_BYTES: u64 = 512;

/// `0x` and the hex of `bytes`, which is how a chain takes raw bytes, a topic or a word.
pub(crate) fn hex0x(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// `0x`-prefixed hex as a block number.
pub(crate) fn parse_block_number(text: &str) -> Option<BlockNumber> {
    u64::from_str_radix(text.strip_prefix("0x")?, 16)
        .ok()
        .map(BlockNumber::new)
}

/// `0x`-prefixed hex as a thirty-two byte word.
pub(crate) fn parse_hash32(value: Option<&Value>) -> Option<[u8; 32]> {
    let text = value?.as_str()?.strip_prefix("0x")?;
    <[u8; 32]>::try_from(hex::decode(text).ok()?).ok()
}

/// The chain's provider url, or the refusal that spends no outcall.
fn url_for(chain_id: ChainId) -> Result<RpcUrl, RpcError> {
    config::get()
        .rpc_urls
        .get(&chain_id)
        .cloned()
        .ok_or(RpcError::NoUrl(chain_id))
}

/// A batch as one request body: an array with one entry per call, numbered from zero in
/// call order, which is what the replies are sorted back into.
fn batch_body(calls: &[(&str, Value)]) -> Vec<u8> {
    let entries: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(id, (method, params))| {
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        })
        .collect();
    Value::Array(entries).to_string().into_bytes()
}

/// The `result` of one reply, or why it is not one. `method` names the call the reply
/// answers, so a refusal out of a batch says which read failed.
fn reply_result(reply: &Value, method: &str) -> Result<Value, RpcError> {
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message")
            .to_string();
        return Err(RpcError::Rpc {
            method: method.to_string(),
            message,
        });
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| RpcError::NeitherResultNorError {
            method: method.to_string(),
        })
}

/// The results of a batch reply, in the order the calls were made, each call answering for
/// itself. One call a provider refuses does not throw away the answers to the others,
/// which is what a batch of broadcasts or receipts needs.
fn parse_batch_each(
    body: &[u8],
    methods: &[&str],
) -> Result<Vec<Result<Value, RpcError>>, RpcError> {
    Ok(sort_replies(body, methods)?
        .into_iter()
        .zip(methods)
        .map(|(reply, method)| reply_result(&reply, method))
        .collect())
}

/// The results of a batch reply, in the order the calls were made, refusing the whole
/// batch on the first call that answered an error.
fn parse_batch(body: &[u8], methods: &[&str]) -> Result<Vec<Value>, RpcError> {
    parse_batch_each(body, methods)?.into_iter().collect()
}

/// The replies of a batch, put back into call order. The server may answer in any order,
/// so the ids are the order: every id from zero to the last must appear exactly once.
fn sort_replies(body: &[u8], methods: &[&str]) -> Result<Vec<Value>, RpcError> {
    let replies: Vec<Value> = serde_json::from_slice(body).map_err(|e| RpcError::NotAnArray {
        calls: methods.len(),
        reason: e.to_string(),
    })?;
    if replies.len() != methods.len() {
        return Err(RpcError::WrongReplyCount {
            asked: methods.len(),
            answered: replies.len(),
        });
    }
    let mut sorted: Vec<Option<&Value>> = vec![None; methods.len()];
    for reply in &replies {
        let id = reply
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| usize::try_from(id).ok())
            .filter(|id| *id < methods.len())
            .ok_or_else(|| RpcError::UnknownReplyId {
                id: reply.get("id").and_then(Value::as_u64),
                calls: methods.len(),
            })?;
        if sorted[id].replace(reply).is_some() {
            return Err(RpcError::DuplicateReplyId { id });
        }
    }
    sorted
        .into_iter()
        .zip(methods)
        .map(|(reply, method)| {
            reply.cloned().ok_or_else(|| RpcError::Unanswered {
                method: method.to_string(),
            })
        })
        .collect()
}

/// Several JSON-RPC calls in ONE request, answered in call order. A batch is how a decision
/// that needs several reads pays for one outcall instead of one per read, and it is the
/// only way those reads see the same moment of the chain. One call answering an error
/// refuses the whole batch, which is what a decision wants.
pub async fn rpc_batch(
    chain_id: ChainId,
    calls: &[(&str, Value)],
    max_bytes: u64,
) -> Result<Vec<Value>, RpcError> {
    let url = url_for(chain_id)?;
    let methods: Vec<&str> = calls.iter().map(|(method, _)| *method).collect();
    let body = http_post(&url, batch_body(calls), max_bytes).await?;
    parse_batch(&body, &methods)
}

/// [`rpc_batch`] where each call answers for itself: the outer error is the transport, and
/// an inner one is that call alone. What a batch of broadcasts needs, where one provider
/// saying "already known" must not throw away the others.
pub async fn rpc_batch_each(
    chain_id: ChainId,
    calls: &[(&str, Value)],
    max_bytes: u64,
) -> Result<Vec<Result<Value, RpcError>>, RpcError> {
    let url = url_for(chain_id)?;
    let methods: Vec<&str> = calls.iter().map(|(method, _)| *method).collect();
    let body = http_post(&url, batch_body(calls), max_bytes).await?;
    parse_batch_each(&body, &methods)
}

impl From<RpcError> for settlement_api::types::rpc::RpcError {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::NoUrl(chain_id) => Self::NoUrl {
                chain_id: chain_id.get(),
            },
            RpcError::Unreachable { reason } => Self::Unreachable {
                reason: reason.as_str().to_string(),
            },
            RpcError::Http { status, body } => Self::Http {
                status,
                body: body.as_str().to_string(),
            },
            RpcError::NotAnArray { calls, reason } => Self::NotAnArray {
                calls: calls as u64,
                reason,
            },
            RpcError::WrongReplyCount { asked, answered } => Self::WrongReplyCount {
                asked: asked as u64,
                answered: answered as u64,
            },
            RpcError::UnknownReplyId { id, calls } => Self::UnknownReplyId {
                id,
                calls: calls as u64,
            },
            RpcError::DuplicateReplyId { id } => Self::DuplicateReplyId { id: id as u64 },
            RpcError::Unanswered { method } => Self::Unanswered { method },
            RpcError::NeitherResultNorError { method } => Self::NeitherResultNorError { method },
            RpcError::Rpc { method, message } => Self::Rpc { method, message },
        }
    }
}

#[cfg(test)]
mod tests;
