use candid::CandidType;
use serde::Deserialize;

/// Why a read of a chain answered nothing usable. `NoUrl` is a deploy mistake, `Rpc` is the
/// provider refusing one call of a batch, and the rest are the transport.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum RpcError {
    NoUrl {
        chain_id: u64,
    },
    /// The system refused the outcall, summarised: a bounded, control-character-free cut
    /// of the reject message, never the whole of it.
    Unreachable {
        reason: String,
    },
    /// The provider answered outside 2xx. The body is summarised the same way, so a
    /// provider cannot answer a caller of this canister with a page of its own.
    Http {
        status: u16,
        body: String,
    },
    /// The body is not the array a batch is answered by.
    NotAnArray {
        calls: u64,
        reason: String,
    },
    /// The provider answered a different number of replies than the batch asked for.
    WrongReplyCount {
        asked: u64,
        answered: u64,
    },
    /// A reply carries an id the batch never used, so nothing says which call it answers.
    UnknownReplyId {
        id: Option<u64>,
        calls: u64,
    },
    /// Two replies carry one id, so one call has two answers and another has none.
    DuplicateReplyId {
        id: u64,
    },
    /// No reply carries this call's id.
    Unanswered {
        method: String,
    },
    /// A reply that is neither a result nor an error is neither an answer nor a refusal.
    NeitherResultNorError {
        method: String,
    },
    Rpc {
        method: String,
        message: String,
    },
}
