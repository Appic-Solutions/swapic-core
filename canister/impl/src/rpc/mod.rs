//! The one door onto a chain: JSON-RPC over an HTTPS outcall.
//!
//! Most reads here are single-provider reads made by one node (`todo_harden_reads`).
//! Unreplicated is what the canister wants for the reasons that hold under either pricing
//! version: one request instead of one per node, so a provider's rate limiter sees one
//! caller, and no transform to agree on. The cycle saving needs
//! [`PRICING_VERSION_PAY_AS_YOU_GO`] as well, because version 1 prices a call by its
//! reserved `max_response_bytes` and ignores the replication mode entirely (measured on
//! mainnet: 87.44M replicated against 87.56M unreplicated).
//!
//! The reads that decide money alone (a pool arrival's receipts, `Swapic.used` before
//! an undo, an Eco intent's `claimants`) are made by every node of the subnet and agreed on
//! ([`rpc_batch_agreed`]): each node's answer goes through [`agreed_response`], which keeps
//! only what the canister reads, and the subnet takes the answer only when enough nodes'
//! agree. Every such read names a fixed block, never `latest`, so honest nodes see the same.
//!
//! `ic-cdk 0.17` carries neither field on its outcall argument, so the argument is built
//! here, as the research canister proved live.

use crate::storage::config;
use candid::{CandidType, Principal};
use ic_cdk::api::call::{call_with_payment128, RejectionCode};
use ic_cdk::api::management_canister::http_request::{
    HttpHeader, HttpMethod, HttpResponse, TransformContext, TransformFunc,
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

/// The canister's query every node runs on its own answer to an agreed read.
pub const AGREED_TRANSFORM: &str = "transform_agreed";

/// What the arguments [`post`] and [`post_agreed`] build must always be: unreplicated with
/// no transform, or replicated through [`AGREED_TRANSFORM`], and priced pay-as-you-go. Held
/// at the one place the call is made, so no future caller can build the argument by hand
/// and lose the policy.
fn assert_outcall_policy(request: &HttpRequest) {
    let transform = request
        .transform
        .as_ref()
        .map(|transform| transform.function.0.method.as_str());
    assert!(
        matches!(
            (request.is_replicated, transform),
            (Some(false), None) | (Some(true), Some(AGREED_TRANSFORM))
        ),
        "BUG: an outcall is unreplicated, or replicated through the agreed transform"
    );
    assert_eq!(
        request.pricing_version,
        Some(PRICING_VERSION_PAY_AS_YOU_GO),
        "BUG: every outcall is priced pay-as-you-go"
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
/// module: enough for ops to tell one failure from another in a log, and never a
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
    #[error("the answer is larger than the {cap} bytes the outcall reserved")]
    AnswerTooLarge { cap: u64 },
}

impl RpcError {
    /// The kind of this failure, which is all that may be recorded where anyone can read
    /// it: the text of an outcall's refusal or a provider's answer can carry the url and
    /// its api key.
    pub fn kind(&self) -> types::books::ReadFailure {
        use types::books::ReadFailure;
        match self {
            RpcError::NoUrl(_) => ReadFailure::NoRpcUrl,
            RpcError::Unreachable { .. } => ReadFailure::Unreachable,
            RpcError::Http { status, .. } => ReadFailure::Http { status: *status },
            RpcError::Rpc { .. } => ReadFailure::Provider,
            RpcError::NotAnArray { .. }
            | RpcError::WrongReplyCount { .. }
            | RpcError::UnknownReplyId { .. }
            | RpcError::DuplicateReplyId { .. }
            | RpcError::Unanswered { .. }
            | RpcError::NeitherResultNorError { .. }
            | RpcError::AnswerTooLarge { .. } => ReadFailure::BadAnswer,
        }
    }
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

/// The argument for one POST to `url` that every node makes, each answer kept by
/// `canister`'s [`AGREED_TRANSFORM`] as `keep` says (one byte per call).
fn post_agreed(
    url: &RpcUrl,
    body: Vec<u8>,
    max_response_bytes: u64,
    keep: &[Keep],
    canister: Principal,
) -> HttpRequest {
    HttpRequest {
        transform: Some(TransformContext {
            function: TransformFunc(candid::Func {
                principal: canister,
                method: AGREED_TRANSFORM.to_string(),
            }),
            context: keep.iter().map(|keep| *keep as u8).collect(),
        }),
        is_replicated: Some(true),
        ..post(url, body, max_response_bytes)
    }
}

/// The cycles to attach to a call every node makes: the replicated formula at the worst
/// response the cap admits and the largest subnet this canister could sit on, doubled, and
/// never below what [`cycles_for`] reserves for the same request (the surplus comes back).
fn cycles_for_agreed(request_bytes: u64, max_response_bytes: u64) -> u128 {
    let n = MAX_SUBNET_NODES;
    let base = (3_000_000 + 60_000 * n) * n;
    let bytes = 400 * n * u128::from(request_bytes) + 800 * n * u128::from(max_response_bytes);
    (2 * (base + bytes)).max(cycles_for(request_bytes, max_response_bytes))
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
    let cycles = cycles_for(request_size(&request), max_response_bytes);
    send(request, cycles).await
}

/// `request`, with `cycles` attached. The body of a 2xx answer, or why there is none. Every
/// call leaves one line in the canister's log ([`Outcall::line`]): where it went, what it
/// asked, how it ended and how far the balance fell over it.
async fn send(request: HttpRequest, cycles: u128) -> Result<Vec<u8>, RpcError> {
    assert_outcall_policy(&request);
    let max_response_bytes = request.max_response_bytes.unwrap_or_default();
    let seen = Outcall::of(&request, cycles);
    let before = balance();
    let answer = call_with_payment128(
        Principal::management_canister(),
        "http_request",
        (request,),
        cycles,
    )
    .await;
    let fell = before.saturating_sub(balance());
    let (response,): (HttpResponse,) = match answer {
        Ok(answer) => answer,
        Err((code, message)) => {
            crate::pool::log(&seen.line(&format!("refused {code:?}: {message}"), fell));
            return Err(refusal(code, &message, max_response_bytes));
        }
    };
    // a status outside a u16 is no http status at all, and is not a 2xx either
    let status: u16 = response.status.0.try_into().unwrap_or(u16::MAX);
    if !(200..300).contains(&status) {
        let body = Summary::of(&String::from_utf8_lossy(&response.body));
        crate::pool::log(&seen.line(&format!("http {status}: {body}"), fell));
        return Err(RpcError::Http { status, body });
    }
    let outcome = format!("ok, {} bytes", response.body.len());
    crate::pool::log(&seen.line(&outcome, fell));
    Ok(response.body)
}

/// The canister's cycle balance; zero off the canister, where a unit test runs.
fn balance() -> u128 {
    #[cfg(target_arch = "wasm32")]
    {
        ic_cdk::api::canister_balance128()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// What the log says of one outcall. It keeps the url only to take it back out of the
/// system's and the provider's text: the url's path carries the provider's key, so a line
/// names the host alone.
struct Outcall {
    url: String,
    host: String,
    methods: String,
    replicated: bool,
    cap: u64,
    attached: u128,
}

impl Outcall {
    fn of(request: &HttpRequest, attached: u128) -> Self {
        Self {
            url: request.url.clone(),
            host: host_of(&request.url).to_string(),
            methods: methods_of(request.body.as_deref().unwrap_or_default()),
            replicated: request.is_replicated == Some(true),
            cap: request.max_response_bytes.unwrap_or_default(),
            attached,
        }
    }

    /// The log line for this call, ended by `outcome` (the url and its key taken out), with
    /// the cycles the balance `fell` by while it was out.
    fn line(&self, outcome: &str, fell: u128) -> String {
        let nodes = if self.replicated { "every node" } else { "one node" };
        format!(
            "outcall {} {} ({}, cap {} bytes): {}; balance fell {} cycles, {} attached",
            self.host,
            self.methods,
            nodes,
            self.cap,
            redact(&Summary::of(outcome).0, &self.url),
            cycles_text(fell),
            cycles_text(self.attached),
        )
    }
}

/// The host of `url`, without its scheme, any user part, its path or its query.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default()
}

/// `text` with `url` taken out, and with its path's every part long enough to be a key:
/// a refusal can quote the whole url, or only its path.
fn redact(text: &str, url: &str) -> String {
    let mut out = text.replace(url, "<url>");
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    if let Some((_, path)) = rest.split_once('/') {
        out = out.replace(path, "<path>");
        for part in path.split(['/', '?', '&', '=', '#']).filter(|part| part.len() >= 8) {
            out = out.replace(part, "<key>");
        }
    }
    out
}

/// The JSON-RPC methods a request body asks, in order, a run of one method as one name and
/// its count.
fn methods_of(body: &[u8]) -> String {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let calls = match &parsed {
        Value::Array(calls) => calls.iter().collect(),
        Value::Object(_) => vec![&parsed],
        _ => Vec::new(),
    };
    let mut runs: Vec<(&str, usize)> = Vec::new();
    for method in calls
        .iter()
        .map(|call| call.get("method").and_then(Value::as_str).unwrap_or("?"))
    {
        match runs.last_mut() {
            Some((last, count)) if *last == method => *count += 1,
            _ => runs.push((method, 1)),
        }
    }
    let names: Vec<String> = runs
        .iter()
        .map(|(method, count)| match count {
            1 => method.to_string(),
            _ => format!("{method} x{count}"),
        })
        .collect();
    match names.is_empty() {
        true => "(no method)".to_string(),
        false => names.join(","),
    }
}

/// A cycle count as ops read it: billions to two places, millions to one.
fn cycles_text(cycles: u128) -> String {
    match cycles {
        c if c >= 1_000_000_000 => {
            format!("{}.{:02}B", c / 1_000_000_000, c % 1_000_000_000 / 10_000_000)
        }
        c if c >= 1_000_000 => format!("{}.{}M", c / 1_000_000, c % 1_000_000 / 100_000),
        c => c.to_string(),
    }
}

/// Why the system refused an outcall reserved at `cap` bytes, from its reject code and
/// message. An answer longer than the cap is refused whole, `SysFatal` with "Http body
/// exceeds size limit of N bytes" (or a "length limit" for the headers): that refusal is
/// typed, as DFINITY's `evm-rpc-canister` reads it (`is_response_too_large`), so a read
/// can ask again with a larger cap. Every other refusal is the system's, summarised.
fn refusal(code: RejectionCode, message: &str, cap: u64) -> RpcError {
    let too_large = message.contains("size limit") || message.contains("length limit");
    if code == RejectionCode::SysFatal && too_large {
        return RpcError::AnswerTooLarge { cap };
    }
    RpcError::Unreachable {
        reason: Summary::of(&format!("{code:?}: {message}")),
    }
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
    // the one url, read in place: no copy of the whole config per outcall
    config::with(|config| config.rpc_urls.get(&chain_id).cloned()).ok_or(RpcError::NoUrl(chain_id))
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

/// The most a block answer may be: a full header with every transaction's hash.
pub const MAX_BLOCK_ANSWER_BYTES: u64 = 256 * 1024;

/// The most a one-block `eth_feeHistory` reply may be: a few hundred bytes and the JSON-RPC
/// envelope around them.
const MAX_FINALIZED_BYTES: u64 = 1_024;

/// The chain's finalized block (the `finalized` tag), as one provider answers it, or the
/// kind of failure: the `oldestBlock` of a one-block fee history ending at the tag, which is
/// that block. The block itself would carry every transaction's hash, and an outcall is
/// charged on its cap.
pub async fn finalized(chain_id: ChainId) -> Result<BlockNumber, types::books::ReadFailure> {
    rpc_batch(
        chain_id,
        &[("eth_feeHistory", json!(["0x1", "finalized", []]))],
        MAX_FINALIZED_BYTES,
    )
    .await
    .map_err(|error| error.kind())?
    .first()
    .and_then(finalized_of)
    .ok_or(types::books::ReadFailure::BadAnswer)
}

/// The block a one-block `eth_feeHistory` answer ending at the `finalized` tag starts at,
/// which is that block.
pub(crate) fn finalized_of(history: &Value) -> Option<BlockNumber> {
    history
        .get("oldestBlock")
        .and_then(Value::as_str)
        .and_then(parse_block_number)
}

/// The chain's head, as one provider answers `eth_blockNumber`, or the kind of failure.
pub async fn head(chain_id: ChainId) -> Result<BlockNumber, types::books::ReadFailure> {
    rpc_batch(
        chain_id,
        &[("eth_blockNumber", json!([]))],
        MAX_BLOCK_NUMBER_BYTES,
    )
    .await
    .map_err(|error| error.kind())?
    .first()
    .and_then(Value::as_str)
    .and_then(parse_block_number)
    .ok_or(types::books::ReadFailure::BadAnswer)
}

/// What an agreed read keeps of one call's answer, so every node's answer reduces to the
/// same bytes whatever else its provider wrote: one byte per call in the transform's
/// context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Keep {
    /// The result as it is: an `eth_call`'s word, a hex number.
    Whole = 0,
    /// A receipt's hash, block, status and logs, each log's emitter, topics, data, index,
    /// `removed` and block time.
    Receipt = 1,
    /// A block header's number, hash and time.
    Header = 2,
    /// A head (`eth_blockNumber`), rounded down to a multiple of [`HEAD_GRAIN`]: nodes that
    /// ask a moment apart see heads a block or two apart, and agree on the rounded one.
    Head = 3,
}

/// The blocks an agreed head is rounded down to a multiple of.
pub const HEAD_GRAIN: u64 = 16;

impl Keep {
    fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Keep::Whole),
            1 => Some(Keep::Receipt),
            2 => Some(Keep::Header),
            3 => Some(Keep::Head),
            _ => None,
        }
    }
}

/// The named fields of `object` that it has, in this order: an answer reduced to what the
/// canister reads.
fn fields(object: &Value, names: &[&str]) -> Value {
    let mut kept = serde_json::Map::new();
    for name in names {
        if let Some(value) = object.get(*name) {
            kept.insert((*name).to_string(), value.clone());
        }
    }
    Value::Object(kept)
}

fn kept(result: &Value, keep: Keep) -> Value {
    match (keep, result) {
        (_, Value::Null) | (Keep::Whole, _) => result.clone(),
        (Keep::Receipt, _) => {
            let mut receipt = fields(result, &["transactionHash", "blockNumber", "status"]);
            let logs: Vec<Value> = result
                .get("logs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|log| {
                    fields(
                        log,
                        &[
                            "address",
                            "topics",
                            "data",
                            "logIndex",
                            "removed",
                            "blockTimestamp",
                        ],
                    )
                })
                .collect();
            if let Value::Object(receipt) = &mut receipt {
                receipt.insert("logs".to_string(), Value::Array(logs));
            }
            receipt
        }
        (Keep::Header, _) => fields(result, &["number", "hash", "timestamp"]),
        (Keep::Head, _) => match result.as_str().and_then(parse_block_number) {
            Some(head) => json!(format!("0x{:x}", head.get() - head.get() % HEAD_GRAIN)),
            None => result.clone(),
        },
    }
}

/// The body of one node's answer to an agreed batch, reduced to what the canister reads:
/// the replies in id order, each its result as `keep` keeps it or its error's code and
/// message, and nothing else a provider writes (headers, timing, its own fields). A body
/// that is not such a batch is left as it is, and nodes agree on it only if they saw the
/// same. Pure, and never traps: it runs on every node.
pub fn agreed_body(body: &[u8], keep: &[Keep]) -> Vec<u8> {
    let Ok(Value::Array(replies)) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let mut sorted: Vec<(u64, Value)> = replies
        .iter()
        .map(|reply| {
            let id = reply.get("id").and_then(Value::as_u64);
            let kept = match (reply.get("result"), reply.get("error")) {
                (Some(result), _) => {
                    let keep = id
                        .and_then(|id| keep.get(usize::try_from(id).ok()?))
                        .copied()
                        .unwrap_or(Keep::Whole);
                    json!({"jsonrpc": "2.0", "id": id, "result": kept(result, keep)})
                }
                (None, Some(error)) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": fields(error, &["code", "message"]),
                }),
                (None, None) => json!({"jsonrpc": "2.0", "id": id}),
            };
            (id.unwrap_or(u64::MAX), kept)
        })
        .collect();
    sorted.sort_by_key(|(id, _)| *id);
    Value::Array(sorted.into_iter().map(|(_, reply)| reply).collect())
        .to_string()
        .into_bytes()
}

/// What [`AGREED_TRANSFORM`] answers for one node's `response`, its context the calls' keep
/// bytes: the status, no headers, and the body [`agreed_body`] keeps (none for an answer
/// that is not a 2xx: its status alone is the refusal). Pure, and never traps.
pub fn agreed_response(response: HttpResponse, context: &[u8]) -> HttpResponse {
    let keep: Vec<Keep> = context
        .iter()
        .map(|byte| Keep::of(*byte).unwrap_or(Keep::Whole))
        .collect();
    let status: u16 = response.status.0.clone().try_into().unwrap_or(u16::MAX);
    let ok = (200..300).contains(&status);
    HttpResponse {
        body: if ok {
            agreed_body(&response.body, &keep)
        } else {
            Vec::new()
        },
        status: response.status,
        headers: Vec::new(),
    }
}

/// [`rpc_batch_each`] made by every node of the subnet and agreed on, each call's
/// answer kept as its [`Keep`] says: what the canister reads decides money, so no one node
/// or one answer may decide it. Every call must name a fixed block, never `latest`, or
/// honest nodes disagree and the read fails. A read the nodes do not agree on fails like
/// any outcall the system refuses, and is asked again later.
pub async fn rpc_batch_agreed(
    chain_id: ChainId,
    calls: &[(&str, Value, Keep)],
    max_bytes: u64,
) -> Result<Vec<Result<Value, RpcError>>, RpcError> {
    let url = url_for(chain_id)?;
    let methods: Vec<&str> = calls.iter().map(|(method, _, _)| *method).collect();
    let keep: Vec<Keep> = calls.iter().map(|(_, _, keep)| *keep).collect();
    let body: Vec<(&str, Value)> = calls
        .iter()
        .map(|(method, params, _)| (*method, params.clone()))
        .collect();
    let request = post_agreed(&url, batch_body(&body), max_bytes, &keep, ic_cdk::api::id());
    let cycles = cycles_for_agreed(request_size(&request), max_bytes);
    let answer = send(request, cycles).await?;
    parse_batch_each(&answer, &methods)
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
            RpcError::AnswerTooLarge { cap } => Self::AnswerTooLarge { cap },
        }
    }
}

#[cfg(test)]
mod tests;
