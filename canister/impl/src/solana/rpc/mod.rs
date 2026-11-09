//! The canister's Solana reads, through the SOL RPC canister and agreed among its providers (design
//! 5.2.3). Only money decisions read Solana here; broadcast is the watcher's. Every read is at
//! `confirmed` (design decision 9).

use candid::{CandidType, Deserialize, Nat, Principal, Reserved};
use ic_cdk::api::call::{call, call_with_payment128};
use swapic_solana_wire::base58;

#[derive(CandidType, Deserialize, Clone, Copy)]
enum SolanaCluster {
    Mainnet,
}

#[derive(CandidType, Deserialize, Clone, Copy)]
enum RpcSources {
    Default(SolanaCluster),
}

#[derive(CandidType, Deserialize, Clone)]
enum ConsensusStrategy {
    Threshold { total: Option<u8>, min: u8 },
}

#[derive(CandidType, Deserialize, Clone)]
#[allow(non_snake_case)]
struct RpcConfig {
    responseSizeEstimate: Option<u64>,
    responseConsensus: Option<ConsensusStrategy>,
}

#[derive(CandidType, Deserialize, Clone, Copy)]
#[allow(non_camel_case_types)]
enum Commitment {
    confirmed,
}

#[derive(CandidType, Deserialize, Clone, Copy)]
#[allow(non_camel_case_types)]
enum Encoding {
    base64,
}

#[derive(CandidType, Deserialize, Clone, Copy)]
struct DataSlice {
    length: u32,
    offset: u32,
}

#[derive(CandidType, Deserialize, Clone)]
#[allow(non_snake_case)]
struct AccountParams {
    pubkey: String,
    commitment: Option<Commitment>,
    encoding: Option<Encoding>,
    dataSlice: Option<DataSlice>,
    minContextSlot: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone)]
#[allow(non_snake_case)]
struct BalanceParams {
    pubkey: String,
    commitment: Option<Commitment>,
    minContextSlot: Option<u64>,
}

#[derive(CandidType, Deserialize, Clone)]
#[allow(non_snake_case)]
struct TransactionParams {
    signature: String,
    commitment: Option<Commitment>,
    maxSupportedTransactionVersion: Option<u8>,
    encoding: Option<Encoding>,
}

/// One provider's answer. The error is not read: any error is no answer.
#[derive(CandidType, Deserialize)]
enum Answer<T> {
    Ok(T),
    Err(Reserved),
}

#[derive(CandidType, Deserialize)]
enum Multi<T> {
    Consistent(Answer<T>),
    Inconsistent(Reserved),
}

#[derive(CandidType, Deserialize)]
enum AccountData {
    #[serde(rename = "binary")]
    Binary(String, Reserved),
    #[serde(rename = "legacyBinary")]
    LegacyBinary(String),
    #[serde(rename = "json")]
    Json(Reserved),
}

#[derive(CandidType, Deserialize)]
struct AccountInfo {
    lamports: u64,
    data: AccountData,
    owner: String,
    space: u64,
}

#[derive(CandidType, Deserialize)]
enum EncodedTransaction {
    #[serde(rename = "binary")]
    Binary(String, Reserved),
    #[serde(rename = "legacyBinary")]
    LegacyBinary(String),
}

#[derive(CandidType, Deserialize)]
enum Instruction {
    #[serde(rename = "compiled")]
    Compiled(CompiledInstruction),
}

#[derive(CandidType, Deserialize)]
#[allow(non_snake_case)]
struct CompiledInstruction {
    data: String,
    programIdIndex: u8,
}

#[derive(CandidType, Deserialize)]
struct InnerInstructions {
    instructions: Vec<Instruction>,
    index: u8,
}

#[derive(CandidType, Deserialize)]
struct LoadedAddresses {
    writable: Vec<String>,
    readonly: Vec<String>,
}

#[derive(CandidType, Deserialize)]
enum Status {
    Ok,
    Err(Reserved),
}

#[derive(CandidType, Deserialize)]
#[allow(non_snake_case)]
struct Meta {
    status: Status,
    innerInstructions: Option<Vec<InnerInstructions>>,
    loadedAddresses: Option<LoadedAddresses>,
}

#[derive(CandidType, Deserialize)]
struct WithMeta {
    meta: Option<Meta>,
    transaction: EncodedTransaction,
}

#[derive(CandidType, Deserialize)]
#[allow(non_snake_case)]
struct Confirmed {
    transaction: WithMeta,
    blockTime: Option<i64>,
}

const SOURCES: RpcSources = RpcSources::Default(SolanaCluster::Mainnet);

fn agreed(size: u64) -> Option<RpcConfig> {
    Some(RpcConfig {
        responseSizeEstimate: Some(size),
        responseConsensus: Some(ConsensusStrategy::Threshold {
            total: None,
            min: 2,
        }),
    })
}

/// Calls `method` with the cycles its `...CyclesCost` query names, plus a tenth (the rest is refunded).
async fn paid_call<P: CandidType + Clone, T: for<'a> Deserialize<'a> + CandidType>(
    sol_rpc: Principal,
    method: &str,
    size: u64,
    params: P,
) -> Result<T, String> {
    let (cost,): (Answer<Nat>,) = call(
        sol_rpc,
        &format!("{method}CyclesCost"),
        (SOURCES, agreed(size), params.clone()),
    )
    .await
    .map_err(|(code, why)| format!("{method}'s cost: {code:?}: {why}"))?;
    let Answer::Ok(cost) = cost else {
        return Err(format!("{method}'s cost was refused"));
    };
    let cost = u128::try_from(cost.0).map_err(|_| format!("{method}'s cost is above u128"))?;
    let (answer,): (Multi<T>,) = call_with_payment128(
        sol_rpc,
        method,
        (SOURCES, agreed(size), params),
        cost + cost / 10,
    )
    .await
    .map_err(|(code, why)| format!("{method}: {code:?}: {why}"))?;
    match answer {
        Multi::Consistent(Answer::Ok(value)) => Ok(value),
        Multi::Consistent(Answer::Err(_)) => {
            Err(format!("{method}: the providers answered an error"))
        }
        Multi::Inconsistent(_) => Err(format!("{method}: the providers do not agree")),
    }
}

fn base64_of(data: AccountData) -> Result<Vec<u8>, String> {
    use base64::Engine;
    match data {
        AccountData::Binary(text, _) => base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|_| "an account's data is not base64".to_string()),
        _ => Err("an account's data is not base64".to_string()),
    }
}

/// An account as a read shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub lamports: u64,
    pub owner: [u8; 32],
    pub space: u64,
    pub data: Vec<u8>,
}

/// An account, or nothing when it does not exist. `first` limits the data read: a read of an account's
/// head does not pay for the rest.
pub async fn account(
    sol_rpc: Principal,
    key: &[u8; 32],
    first: Option<u32>,
    size: u64,
) -> Result<Option<Account>, String> {
    let params = AccountParams {
        pubkey: base58::encode(key),
        commitment: Some(Commitment::confirmed),
        encoding: Some(Encoding::base64),
        dataSlice: first.map(|length| DataSlice { length, offset: 0 }),
        minContextSlot: None,
    };
    let info: Option<AccountInfo> = paid_call(sol_rpc, "getAccountInfo", size, params).await?;
    let Some(info) = info else {
        return Ok(None);
    };
    Ok(Some(Account {
        lamports: info.lamports,
        owner: base58::decode_pubkey(&info.owner).ok_or("an account's owner is not base58")?,
        space: info.space,
        data: base64_of(info.data)?,
    }))
}

pub async fn balance(sol_rpc: Principal, key: &[u8; 32]) -> Result<u64, String> {
    let params = BalanceParams {
        pubkey: base58::encode(key),
        commitment: Some(Commitment::confirmed),
        minContextSlot: None,
    };
    paid_call(sol_rpc, "getBalance", 512, params).await
}

/// A landed transaction as a read shows it: the message's bytes, what it loaded, what it did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct LandedTx {
    pub failed: bool,
    /// The whole signed transaction's bytes.
    pub wire: Vec<u8>,
    pub loaded_writable: Vec<[u8; 32]>,
    pub loaded_readonly: Vec<[u8; 32]>,
    /// By the top-level instruction's index: each inner instruction's program index and data.
    pub inner: Vec<(u8, Vec<(u8, Vec<u8>)>)>,
    pub block_time_s: Option<u64>,
}

/// The transaction with this signature, or nothing when no agreed provider has it.
pub async fn transaction(
    sol_rpc: Principal,
    signature: &[u8; 64],
) -> Result<Option<LandedTx>, String> {
    use base64::Engine;
    let params = TransactionParams {
        signature: base58::encode(signature),
        commitment: Some(Commitment::confirmed),
        maxSupportedTransactionVersion: Some(0),
        encoding: Some(Encoding::base64),
    };
    let answer: Option<Confirmed> = paid_call(sol_rpc, "getTransaction", 48 * 1024, params).await?;
    let Some(confirmed) = answer else {
        return Ok(None);
    };
    let meta = confirmed
        .transaction
        .meta
        .ok_or("a transaction with no meta")?;
    let wire = match confirmed.transaction.transaction {
        EncodedTransaction::Binary(text, _) => base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|_| "a transaction that is not base64")?,
        EncodedTransaction::LegacyBinary(_) => {
            return Err("a transaction in the legacy encoding".to_string())
        }
    };
    let keys = |list: Vec<String>| -> Result<Vec<[u8; 32]>, String> {
        list.iter()
            .map(|k| {
                base58::decode_pubkey(k).ok_or_else(|| "a loaded address is not base58".to_string())
            })
            .collect()
    };
    let (loaded_writable, loaded_readonly) = match meta.loadedAddresses {
        Some(loaded) => (keys(loaded.writable)?, keys(loaded.readonly)?),
        None => (Vec::new(), Vec::new()),
    };
    let mut inner = Vec::new();
    for group in meta.innerInstructions.unwrap_or_default() {
        let mut instructions = Vec::with_capacity(group.instructions.len());
        for Instruction::Compiled(ix) in group.instructions {
            let data =
                base58::decode(&ix.data).ok_or("an inner instruction's data is not base58")?;
            instructions.push((ix.programIdIndex, data));
        }
        inner.push((group.index, instructions));
    }
    Ok(Some(LandedTx {
        failed: matches!(meta.status, Status::Err(_)),
        wire,
        loaded_writable,
        loaded_readonly,
        inner,
        block_time_s: confirmed.blockTime.and_then(|t| u64::try_from(t).ok()),
    }))
}

#[cfg(test)]
mod tests;
