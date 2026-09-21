use candid::CandidType;
use serde::Deserialize;

/// Why a read of a chain answered nothing usable. `NoUrl` is a deploy mistake, `Rpc` is the
/// provider refusing one call of a batch, and the rest are the transport.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RpcError {
    NoUrl { chain_id: u64 },
    Unreachable { message: String },
    Http { status: u16, body: String },
    Json { message: String },
    Rpc { method: String, message: String },
}
