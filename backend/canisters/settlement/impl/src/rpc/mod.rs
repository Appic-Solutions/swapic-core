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
use types::{ChainId, RpcUrl};

/// Pay-as-you-go pricing: charges what the call consumed rather than what it reserved, and
/// it is the only version that prices an unreplicated call as one.
pub const PRICING_VERSION_PAY_AS_YOU_GO: u32 = 2;

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
#[derive(CandidType, Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub max_response_bytes: Option<u64>,
    pub method: HttpMethod,
    pub headers: Vec<HttpHeader>,
    pub body: Option<Vec<u8>>,
    pub transform: Option<TransformContext>,
    /// `Some(false)`: one replica makes the request, not all of them.
    pub is_replicated: Option<bool>,
    /// `opt nat32` on the wire, per interface specification 0.68.0.
    pub pricing_version: Option<u32>,
}

/// Why a read of a chain answered nothing usable.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RpcError {
    #[error("no rpc url is configured for chain {0}")]
    NoUrl(ChainId),
    #[error("the outcall was rejected: {message}")]
    Unreachable { message: String },
    #[error("the provider answered http {status}")]
    Http { status: u16, body: String },
    #[error("the answer is not the json-rpc this canister reads: {0}")]
    Json(String),
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
pub async fn http_post(
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
        message: format!("{code:?}: {message}"),
    })?;
    // a status outside a u16 is no http status at all, and is not a 2xx either
    let status: u16 = response.status.0.try_into().unwrap_or(u16::MAX);
    if !(200..300).contains(&status) {
        return Err(RpcError::Http {
            status,
            body: String::from_utf8_lossy(&response.body).into_owned(),
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

/// The chain's provider url, or the refusal that spends no outcall.
fn url_for(chain_id: ChainId) -> Result<RpcUrl, RpcError> {
    config::get()
        .rpc_urls
        .get(&chain_id)
        .cloned()
        .ok_or(RpcError::NoUrl(chain_id))
}

/// One JSON-RPC call as a request body, numbered zero.
fn single_body(method: &str, params: &Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params})
        .to_string()
        .into_bytes()
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
        .ok_or_else(|| RpcError::Json(format!("{method} answered neither a result nor an error")))
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
    let replies: Vec<Value> = serde_json::from_slice(body).map_err(|e| {
        RpcError::Json(format!(
            "a batch of {} is answered by an array: {e}",
            methods.len()
        ))
    })?;
    if replies.len() != methods.len() {
        return Err(RpcError::Json(format!(
            "{} calls were answered by {} replies",
            methods.len(),
            replies.len()
        )));
    }
    let mut sorted: Vec<Option<&Value>> = vec![None; methods.len()];
    for reply in &replies {
        let id = reply
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| usize::try_from(id).ok())
            .filter(|id| *id < methods.len())
            .ok_or_else(|| {
                RpcError::Json(format!(
                    "a reply carries no id this batch asked for: {reply}"
                ))
            })?;
        if sorted[id].replace(reply).is_some() {
            return Err(RpcError::Json(format!("id {id} was answered twice")));
        }
    }
    sorted
        .into_iter()
        .zip(methods)
        .map(|(reply, method)| {
            reply
                .cloned()
                .ok_or_else(|| RpcError::Json(format!("{method} was not answered")))
        })
        .collect()
}

/// The result of a single reply.
fn parse_single(body: &[u8], method: &str) -> Result<Value, RpcError> {
    let reply: Value = serde_json::from_slice(body)
        .map_err(|e| RpcError::Json(format!("{method} is answered by an object: {e}")))?;
    reply_result(&reply, method)
}

/// One JSON-RPC call on a chain. `max_bytes` is the cap on the answer, and it is the
/// caller's job to make it tight.
pub async fn rpc_one(
    chain_id: ChainId,
    method: &str,
    params: Value,
    max_bytes: u64,
) -> Result<Value, RpcError> {
    let url = url_for(chain_id)?;
    let body = http_post(&url, single_body(method, &params), max_bytes).await?;
    parse_single(&body, method)
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
            RpcError::Unreachable { message } => Self::Unreachable { message },
            RpcError::Http { status, body } => Self::Http { status, body },
            RpcError::Json(message) => Self::Json { message },
            RpcError::Rpc { method, message } => Self::Rpc { method, message },
        }
    }
}

#[cfg(test)]
mod tests;
