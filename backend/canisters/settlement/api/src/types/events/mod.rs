use crate::types::codec::{put_bytes, put_str};
use candid::CandidType;
use serde::{Deserialize, Serialize};
use sha2::Digest;

pub type Hash32 = [u8; 32];

// update together with the enum and samples(); the exhaustive match in event_bytes is the
// compile-time check, this is the golden-count check
pub const EVENT_VARIANT_COUNT: usize = 19;

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub enum Event {
    ConfigChanged {
        json: String,
    },
    FundsReceived {
        quote_hash: Hash32,
        quote_bytes: Vec<u8>,
        chain_id: u64,
        token: String,
        amount: u128,
        tx_ref: String,
    },
    TxSigned {
        quote_hash: Hash32,
        attempt: u32,
        chain_id: u64,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    TxConfirmed {
        quote_hash: Hash32,
        attempt: u32,
        chain_id: u64,
        tx_hash: Hash32,
        block: u64,
    },
    TxFailed {
        quote_hash: Hash32,
        attempt: u32,
        reason: String,
    },
    PaidInStable {
        quote_hash: Hash32,
        chain_id: u64,
        amount: u128,
    },
    DecisionRequired {
        quote_hash: Hash32,
        reason: String,
    },
    DecisionMade {
        quote_hash: Hash32,
        choice: Choice,
    },
    RefundStarted {
        quote_hash: Hash32,
        reason: String,
    },
    Refunded {
        quote_hash: Hash32,
        chain_id: u64,
        token: String,
        amount: u128,
        to: String,
    },
    SwapDone {
        quote_hash: Hash32,
    },
    Frozen {
        quote_hash: Hash32,
        reason: String,
    },
    FeeAccrued {
        quote_hash: Hash32,
        amount: u128,
    },
    PocketFunded {
        chain_id: u64,
        amount: u128,
    },
    PocketReserved {
        quote_hash: Hash32,
        chain_id: u64,
        amount: u128,
    },
    PocketRebalanced {
        from_chain: u64,
        to_chain: u64,
        amount: u128,
        route: String,
    },
    PocketReleased {
        quote_hash: Hash32,
        chain_id: u64,
        amount: u128,
    },
    PocketSpent {
        quote_hash: Hash32,
        chain_id: u64,
        amount: u128,
    },
    /// Principals as text, so the audit line reads without a decoder.
    RolesChanged {
        quoter: String,
        watcher: String,
    },
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub enum Choice {
    Requote,
    Refund,
}

#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    pub index: u64,
    pub time_ns: u64,
    pub parent_hash: Hash32,
    pub hash: Hash32,
    pub event: Event,
}

// Canonical hash preimage. Primitives: u16/u32/u64 big-endian, u128 big-endian 16 bytes,
// Hash32 raw, String and Vec<u8> as u32-be length then bytes, bool one byte 0/1,
// Choice one byte. Tags below are assigned once: never renumber, never reuse a retired
// tag, only append. Nothing here may depend on candid or on the shape of the enum.
fn put_tag(b: &mut Vec<u8>, tag: u16) {
    b.extend_from_slice(&tag.to_be_bytes());
}

pub fn event_bytes(event: &Event) -> Vec<u8> {
    let mut b = Vec::new();
    // exhaustive, no wildcard arm: a new variant fails to compile until the codec covers it
    match event {
        Event::ConfigChanged { json } => {
            put_tag(&mut b, 0);
            put_str(&mut b, json);
        }
        Event::FundsReceived {
            quote_hash,
            quote_bytes,
            chain_id,
            token,
            amount,
            tx_ref,
        } => {
            put_tag(&mut b, 1);
            b.extend_from_slice(quote_hash);
            put_bytes(&mut b, quote_bytes);
            b.extend_from_slice(&chain_id.to_be_bytes());
            put_str(&mut b, token);
            b.extend_from_slice(&amount.to_be_bytes());
            put_str(&mut b, tx_ref);
        }
        Event::TxSigned {
            quote_hash,
            attempt,
            chain_id,
            tx_hash,
            raw_tx,
        } => {
            put_tag(&mut b, 2);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&attempt.to_be_bytes());
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(tx_hash);
            put_bytes(&mut b, raw_tx);
        }
        Event::TxConfirmed {
            quote_hash,
            attempt,
            chain_id,
            tx_hash,
            block,
        } => {
            put_tag(&mut b, 3);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&attempt.to_be_bytes());
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(tx_hash);
            b.extend_from_slice(&block.to_be_bytes());
        }
        Event::TxFailed {
            quote_hash,
            attempt,
            reason,
        } => {
            put_tag(&mut b, 4);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&attempt.to_be_bytes());
            put_str(&mut b, reason);
        }
        Event::PaidInStable {
            quote_hash,
            chain_id,
            amount,
        } => {
            put_tag(&mut b, 5);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::DecisionRequired { quote_hash, reason } => {
            put_tag(&mut b, 6);
            b.extend_from_slice(quote_hash);
            put_str(&mut b, reason);
        }
        Event::DecisionMade { quote_hash, choice } => {
            put_tag(&mut b, 7);
            b.extend_from_slice(quote_hash);
            b.push(match choice {
                Choice::Requote => 0,
                Choice::Refund => 1,
            });
        }
        Event::RefundStarted { quote_hash, reason } => {
            put_tag(&mut b, 8);
            b.extend_from_slice(quote_hash);
            put_str(&mut b, reason);
        }
        Event::Refunded {
            quote_hash,
            chain_id,
            token,
            amount,
            to,
        } => {
            put_tag(&mut b, 9);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&chain_id.to_be_bytes());
            put_str(&mut b, token);
            b.extend_from_slice(&amount.to_be_bytes());
            put_str(&mut b, to);
        }
        Event::SwapDone { quote_hash } => {
            put_tag(&mut b, 10);
            b.extend_from_slice(quote_hash);
        }
        Event::Frozen { quote_hash, reason } => {
            put_tag(&mut b, 11);
            b.extend_from_slice(quote_hash);
            put_str(&mut b, reason);
        }
        Event::FeeAccrued { quote_hash, amount } => {
            put_tag(&mut b, 12);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::PocketFunded { chain_id, amount } => {
            put_tag(&mut b, 13);
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::PocketReserved {
            quote_hash,
            chain_id,
            amount,
        } => {
            put_tag(&mut b, 14);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::PocketRebalanced {
            from_chain,
            to_chain,
            amount,
            route,
        } => {
            put_tag(&mut b, 15);
            b.extend_from_slice(&from_chain.to_be_bytes());
            b.extend_from_slice(&to_chain.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
            put_str(&mut b, route);
        }
        Event::PocketReleased {
            quote_hash,
            chain_id,
            amount,
        } => {
            put_tag(&mut b, 16);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::PocketSpent {
            quote_hash,
            chain_id,
            amount,
        } => {
            put_tag(&mut b, 17);
            b.extend_from_slice(quote_hash);
            b.extend_from_slice(&chain_id.to_be_bytes());
            b.extend_from_slice(&amount.to_be_bytes());
        }
        Event::RolesChanged { quoter, watcher } => {
            put_tag(&mut b, 18);
            put_str(&mut b, quoter);
            put_str(&mut b, watcher);
        }
    }
    b
}

pub fn event_hash(index: u64, time_ns: u64, parent_hash: &Hash32, event: &Event) -> Hash32 {
    let mut h = sha2::Sha256::new();
    h.update(index.to_be_bytes());
    h.update(time_ns.to_be_bytes());
    h.update(parent_hash);
    // the codec is hand-written so hashes never depend on candid or enum shape;
    // candid remains for storage and API
    h.update(event_bytes(event));
    h.finalize().into()
}

pub fn seal(index: u64, time_ns: u64, parent_hash: Hash32, event: Event) -> EventEnvelope {
    let hash = event_hash(index, time_ns, &parent_hash, &event);
    EventEnvelope {
        index,
        time_ns,
        parent_hash,
        hash,
        event,
    }
}

/// Genesis-anchored: index 0 must link to the zero hash and every link after it must
/// hold. Takes anything iterable, by value or by reference, so a stable log can be
/// checked as a stream and a slice can be checked in place: one copy of the rule.
pub fn chain_is_valid<E: std::borrow::Borrow<EventEnvelope>>(
    events: impl IntoIterator<Item = E>,
) -> bool {
    let mut parent = [0u8; 32];
    for (i, e) in events.into_iter().enumerate() {
        let e = e.borrow();
        if e.index != i as u64 || e.parent_hash != parent {
            return false;
        }
        if e.hash != event_hash(e.index, e.time_ns, &e.parent_hash, &e.event) {
            return false;
        }
        parent = e.hash;
    }
    true
}

#[cfg(test)]
mod tests;
