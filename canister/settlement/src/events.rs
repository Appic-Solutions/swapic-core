use candid::CandidType;
use serde::{Deserialize, Serialize};
use sha2::Digest;

pub type Hash32 = [u8; 32];

// update together with the enum and samples(); the exhaustive match in event_bytes is the
// compile-time check, this is the golden-count check
pub const EVENT_VARIANT_COUNT: usize = 17;

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
fn put_len(b: &mut Vec<u8>, len: usize) {
    let n = u32::try_from(len).expect("field length fits u32");
    b.extend_from_slice(&n.to_be_bytes());
}

fn put_bytes(b: &mut Vec<u8>, v: &[u8]) {
    put_len(b, v.len());
    b.extend_from_slice(v);
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_bytes(b, s.as_bytes());
}

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

    /// One fixed sample of every variant, in tag order, for the golden file.
    fn samples() -> Vec<Event> {
        vec![
            Event::ConfigChanged {
                json: "{\"fee_bps\":30}".into(),
            },
            Event::FundsReceived {
                quote_hash: [1; 32],
                quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
                chain_id: 8453,
                token: "USDC".into(),
                amount: 1_000_000,
                tx_ref: "0xfeed".into(),
            },
            Event::TxSigned {
                quote_hash: [2; 32],
                attempt: 1,
                chain_id: 42161,
                tx_hash: [3; 32],
                raw_tx: vec![0x02, 0xf8, 0x6b],
            },
            Event::TxConfirmed {
                quote_hash: [4; 32],
                attempt: 2,
                chain_id: 1,
                tx_hash: [5; 32],
                block: 19_000_000,
            },
            Event::TxFailed {
                quote_hash: [6; 32],
                attempt: 3,
                reason: "reverted".into(),
            },
            Event::PaidInStable {
                quote_hash: [7; 32],
                chain_id: 8453,
                amount: 999_999,
            },
            Event::DecisionRequired {
                quote_hash: [8; 32],
                reason: "slippage".into(),
            },
            Event::DecisionMade {
                quote_hash: [9; 32],
                choice: Choice::Refund,
            },
            Event::RefundStarted {
                quote_hash: [10; 32],
                reason: "timeout".into(),
            },
            Event::Refunded {
                quote_hash: [11; 32],
                chain_id: 137,
                token: "USDT".into(),
                amount: 42,
                // empty string: length prefix must still be written
                to: String::new(),
            },
            Event::SwapDone {
                quote_hash: [12; 32],
            },
            Event::Frozen {
                quote_hash: [13; 32],
                // multibyte utf8: length is bytes, not chars
                reason: "griefed \u{2603}".into(),
            },
            Event::FeeAccrued {
                quote_hash: [14; 32],
                amount: 7,
            },
            Event::PocketFunded {
                chain_id: 10,
                // high word set: proves the u128 is 16 bytes, not a truncated u64
                amount: 1u128 << 70,
            },
            Event::PocketReserved {
                quote_hash: [15; 32],
                chain_id: 8453,
                amount: 3,
            },
            Event::PocketRebalanced {
                from_chain: 8453,
                to_chain: 42161,
                amount: 250,
                route: "cctp".into(),
            },
            Event::PocketReleased {
                quote_hash: [16; 32],
                chain_id: 8453,
                amount: 150,
            },
        ]
    }

    const GOLDEN: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/event_bytes_v1.txt"
    );

    fn tag_of(line: &str) -> &str {
        line.get(..4).unwrap_or("????")
    }

    #[test]
    fn choice_encodes_one_byte() {
        let q = event_bytes(&Event::DecisionMade {
            quote_hash: [0; 32],
            choice: Choice::Requote,
        });
        let r = event_bytes(&Event::DecisionMade {
            quote_hash: [0; 32],
            choice: Choice::Refund,
        });
        assert_eq!(q.len(), 35, "tag + hash + one choice byte");
        assert_eq!(q.len(), r.len());
        assert_eq!(*q.last().unwrap(), 0, "Requote encodes 0");
        assert_eq!(*r.last().unwrap(), 1, "Refund encodes 1");
    }

    #[test]
    fn event_bytes_matches_golden_vectors() {
        let s = samples();
        assert_eq!(
            s.len(),
            EVENT_VARIANT_COUNT,
            "samples() must cover every variant"
        );
        let got: Vec<String> = s.iter().map(|e| hex::encode(event_bytes(e))).collect();

        // regeneration is opt-in and never green, so a blessing is always a deliberate diff
        if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
            let dir = std::path::Path::new(GOLDEN).parent().unwrap();
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(GOLDEN, got.join("\n") + "\n").unwrap();
            panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
        }

        let raw = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
            panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
        });
        let want: Vec<&str> = raw.lines().collect();
        assert_eq!(
            want.len(),
            got.len(),
            "golden has {} lines but samples() has {}: {GOLDEN}",
            want.len(),
            got.len()
        );
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(
                g,
                w,
                "canonical layout changed: breaking. first differing line {i} (tag {})",
                tag_of(g)
            );
        }
    }

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
