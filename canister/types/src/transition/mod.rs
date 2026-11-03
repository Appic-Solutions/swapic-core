//! Why an event cannot move the fold it was offered to: the nonce allocator's rules, and the
//! refusals of a flow, a chain's books and the pool.

use crate::chain::ChainId;
use crate::hash::QuoteHash;
use crate::numeric::Nonce;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("nonce {nonce} on chain {chain_id} is not the {expected} the allocator is at")]
    NonceOutOfSequence {
        chain_id: ChainId,
        nonce: Nonce,
        expected: Nonce,
    },
    #[error("nonce {nonce} on chain {chain_id} was never allocated: the allocator is at {next}")]
    NonceNeverAllocated {
        chain_id: ChainId,
        nonce: Nonce,
        next: Nonce,
    },
    #[error("the allocator on chain {chain_id} has no number left to hand out")]
    NonceExhausted { chain_id: ChainId },
    #[error("{0} already holds a nonce it has not signed for")]
    NonceStillUnsigned(QuoteHash),
    #[error("nonce {nonce} on chain {chain_id} is not one that is waiting for a signature")]
    NonceNotUnsigned { chain_id: ChainId, nonce: Nonce },
    #[error(transparent)]
    Flow(#[from] crate::flow::FlowError),
    #[error(transparent)]
    Books(#[from] crate::books::BooksError),
    #[error(transparent)]
    Pool(#[from] crate::pool::PoolStepError),
}
