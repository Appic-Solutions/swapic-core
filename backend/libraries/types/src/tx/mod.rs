//! The outbox: what the canister has signed and still has to get onto a chain.
//!
//! An entry is keyed by the nonce it holds, so a replacement at the same nonce overwrites
//! the entry it replaces and two transactions can never be in flight for one nonce. It is
//! NOT part of the fold of the event log: the log records that a transaction was created,
//! signed and replaced, while how far its broadcast got is work in progress that no event
//! describes, so the replay audit does not compare it.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::events::TxPurpose;
use crate::evm::EvmAddress;
use crate::hash::TxHash;
use crate::numeric::{
    Attempt, BlockDepth, BlockNumber, GasAmount, Nonce, Timestamp, Wei, WeiPerGas,
};
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
use std::time::Duration;

/// How far one outbound transaction got.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum OutboxStatus {
    /// Signed and recorded, not yet handed to a provider.
    #[n(0)]
    Queued,
    /// Handed to a provider at least once, with no receipt deep enough yet.
    #[n(1)]
    Sent,
}

/// One chain's nonce: what an outbox entry is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutboxKey {
    pub chain_id: ChainId,
    pub nonce: Nonce,
}

/// Sixteen bytes: the chain id then the nonce, both big-endian, so a walk of the map meets
/// each chain's transactions in the order they were allocated.
impl Storable for OutboxKey {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&self.chain_id.get().to_be_bytes());
        bytes.extend_from_slice(&self.nonce.get().to_be_bytes());
        Cow::Owned(bytes)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let (chain_id, nonce) = bytes
            .split_first_chunk::<8>()
            .expect("BUG: a stored outbox key is written as exactly 16 bytes");
        Self {
            chain_id: ChainId::new(u64::from_be_bytes(*chain_id)),
            nonce: Nonce::new(u64::from_be_bytes(
                nonce
                    .try_into()
                    .expect("BUG: a stored outbox key is written as exactly 16 bytes"),
            )),
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: 16,
        is_fixed_size: true,
    };
}

/// One signed transaction on its way to a chain.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct OutboxEntry {
    #[n(0)]
    pub purpose: TxPurpose,
    #[n(1)]
    pub chain_id: ChainId,
    #[n(2)]
    pub nonce: Nonce,
    /// The swap's attempt this transaction is, for the transactions that belong to a swap.
    #[n(3)]
    pub attempt: Option<Attempt>,
    /// Every transaction broadcast at this nonce, oldest first, the last being the current
    /// one. A replacement does not unsend what it replaces, so any of them may be the one
    /// that lands and every one of them is looked up.
    #[n(4)]
    pub hashes: Vec<TxHash>,
    /// The bytes of the current transaction, exactly as they were signed.
    #[cbor(n(5), with = "minicbor::bytes")]
    pub raw_tx: Vec<u8>,
    /// What the transaction calls, kept beside the signed bytes so a replacement re-signs
    /// the same call at a new fee without decoding an envelope back.
    #[n(11)]
    pub to: EvmAddress,
    #[n(12)]
    pub value: Wei,
    #[cbor(n(13), with = "minicbor::bytes")]
    pub data: Vec<u8>,
    #[n(14)]
    pub gas_limit: GasAmount,
    #[n(6)]
    pub max_fee: WeiPerGas,
    #[n(7)]
    pub max_priority_fee: WeiPerGas,
    #[n(8)]
    pub status: OutboxStatus,
    #[n(9)]
    pub created_at: Timestamp,
    /// When the current bytes were last handed to a provider.
    #[n(10)]
    pub last_sent_at: Option<Timestamp>,
}

impl OutboxEntry {
    pub fn key(&self) -> OutboxKey {
        OutboxKey {
            chain_id: self.chain_id,
            nonce: self.nonce,
        }
    }

    /// The transaction currently broadcast at this nonce.
    pub fn tx_hash(&self) -> TxHash {
        *self
            .hashes
            .last()
            .expect("BUG: an outbox entry is created with the hash of its transaction")
    }

    /// How long the current bytes have been out with a provider, or nothing while they
    /// have never been sent.
    pub fn sent_for(&self, now: Timestamp) -> Option<Duration> {
        self.last_sent_at
            .map(|sent| now.saturating_duration_since(sent))
    }

    /// Records that the current bytes went out at `at`.
    pub fn sent(&mut self, at: Timestamp) {
        self.status = OutboxStatus::Sent;
        self.last_sent_at = Some(at);
    }

    /// Takes the same nonce to a new transaction at a higher fee: the old hash stays, so a
    /// receipt for it is still recognised, and the new bytes go out on the next pass.
    pub fn replaced(
        &self,
        hash: TxHash,
        raw_tx: Vec<u8>,
        max_fee: WeiPerGas,
        max_priority_fee: WeiPerGas,
    ) -> Self {
        let mut hashes = self.hashes.clone();
        hashes.push(hash);
        Self {
            hashes,
            raw_tx,
            max_fee,
            max_priority_fee,
            status: OutboxStatus::Queued,
            last_sent_at: None,
            ..self.clone()
        }
    }
}

/// Whether a receipt in `block` is deep enough against a head at `latest`. The receipt's
/// own block is the first confirmation, so a depth of one is satisfied by the head itself.
/// A receipt from a block the head has not reached is a provider answering from two
/// different moments, and is not deep enough.
pub fn is_confirmed(block: BlockNumber, latest: BlockNumber, depth: BlockDepth) -> bool {
    let confirmations = latest
        .get()
        .checked_sub(block.get())
        .and_then(|behind| behind.checked_add(1));
    confirmations.is_some_and(|confirmations| confirmations >= depth.get().max(1))
}

crate::storable_as_cbor!(OutboxEntry);
