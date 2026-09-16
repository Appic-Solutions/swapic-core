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
        Event::PocketSpent {
            quote_hash: [17; 32],
            chain_id: 42161,
            amount: 250,
        },
        Event::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        },
    ]
}

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/event_bytes_v1.txt");

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

// Storage is candid, and a variant that fails to decode would trap post_upgrade and
// strand the canister on its current wasm. This walks the same Storable impl that
// replay runs, over every variant, so the append-only storage rule is CI-enforced.
#[test]
fn every_variant_round_trips_through_storage() {
    use ic_stable_structures::Storable;

    let s = samples();
    assert_eq!(
        s.len(),
        EVENT_VARIANT_COUNT,
        "samples() must cover every variant"
    );
    for (i, event) in s.into_iter().enumerate() {
        let env = seal(i as u64, 1_700_000_000, [i as u8; 32], event);
        let back = EventEnvelope::from_bytes(env.to_bytes());
        assert_eq!(back, env, "variant {i} does not survive stable storage");
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
