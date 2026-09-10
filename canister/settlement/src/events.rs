use candid::CandidType;
use serde::{Deserialize, Serialize};
use sha2::Digest;

pub type Hash32 = [u8; 32];

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

pub fn event_hash(index: u64, time_ns: u64, parent_hash: &Hash32, event: &Event) -> Hash32 {
    let mut h = sha2::Sha256::new();
    h.update(index.to_be_bytes());
    h.update(time_ns.to_be_bytes());
    h.update(parent_hash);
    // pinned candid version makes this encoding stable; the pin is a consensus rule
    h.update(candid::encode_one(event).expect("event encodes"));
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

pub fn chain_is_valid(events: &[EventEnvelope]) -> bool {
    let mut parent = [0u8; 32];
    for (i, e) in events.iter().enumerate() {
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
mod tests {
    use super::*;

    #[test]
    fn hash_changes_when_any_field_changes() {
        let a = event_hash(
            1,
            2,
            &[0; 32],
            &Event::SwapDone {
                quote_hash: [1; 32],
            },
        );
        assert_ne!(
            a,
            event_hash(
                2,
                2,
                &[0; 32],
                &Event::SwapDone {
                    quote_hash: [1; 32]
                }
            )
        );
        assert_ne!(
            a,
            event_hash(
                1,
                3,
                &[0; 32],
                &Event::SwapDone {
                    quote_hash: [1; 32]
                }
            )
        );
        assert_ne!(
            a,
            event_hash(
                1,
                2,
                &[9; 32],
                &Event::SwapDone {
                    quote_hash: [1; 32]
                }
            )
        );
        assert_ne!(
            a,
            event_hash(
                1,
                2,
                &[0; 32],
                &Event::SwapDone {
                    quote_hash: [2; 32]
                }
            )
        );
    }

    #[test]
    fn chain_links_and_detects_tampering() {
        let e0 = seal(
            0,
            100,
            [0; 32],
            Event::PocketFunded {
                chain_id: 8453,
                amount: 5,
            },
        );
        let e1 = seal(
            1,
            200,
            e0.hash,
            Event::SwapDone {
                quote_hash: [1; 32],
            },
        );
        assert!(chain_is_valid(&[e0.clone(), e1.clone()]));
        let mut bad = e1.clone();
        bad.event = Event::SwapDone {
            quote_hash: [2; 32],
        };
        assert!(!chain_is_valid(&[e0.clone(), bad]));
        let mut unlinked = e1;
        unlinked.parent_hash = [7; 32];
        assert!(!chain_is_valid(&[e0, unlinked]));
    }
}
