use crate::types::events::Hash32;
use candid::CandidType;
use serde::Deserialize;

/// Why an event cannot move the state it was offered to.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    NonceOutOfSequence {
        chain_id: u64,
        nonce: u64,
        expected: u64,
    },
    NonceNeverAllocated {
        chain_id: u64,
        nonce: u64,
        next: u64,
    },
    NonceExhausted {
        chain_id: u64,
    },
    NonceStillUnsigned(Hash32),
    NonceNotUnsigned {
        chain_id: u64,
        nonce: u64,
    },
    /// A flow step refused, with the domain's reason.
    Flow(String),
    Books(String),
    Pool(String),
}

impl From<types::TransitionError> for TransitionError {
    fn from(error: types::TransitionError) -> Self {
        use types::TransitionError as Domain;
        match error {
            Domain::NonceOutOfSequence {
                chain_id,
                nonce,
                expected,
            } => Self::NonceOutOfSequence {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                expected: expected.get(),
            },
            Domain::NonceNeverAllocated {
                chain_id,
                nonce,
                next,
            } => Self::NonceNeverAllocated {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                next: next.get(),
            },
            Domain::NonceExhausted { chain_id } => Self::NonceExhausted {
                chain_id: chain_id.get(),
            },
            Domain::NonceStillUnsigned(quote_hash) => {
                Self::NonceStillUnsigned(quote_hash.into_bytes())
            }
            Domain::NonceNotUnsigned { chain_id, nonce } => Self::NonceNotUnsigned {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
            },
            Domain::Flow(error) => Self::Flow(error.to_string()),
            Domain::Books(error) => Self::Books(error.to_string()),
            Domain::Pool(error) => Self::Pool(error.to_string()),
        }
    }
}
