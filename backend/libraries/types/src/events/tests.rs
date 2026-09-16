use super::*;

fn quote(byte: u8) -> QuoteHash {
    QuoteHash::new([byte; 32])
}

fn text<T: std::str::FromStr>(s: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    s.parse().unwrap()
}

fn amount(value: u128) -> TokenAmount {
    TokenAmount::from(value)
}

/// One fixed sample of every variant, in tag order, for the golden file.
fn samples() -> Vec<EventType> {
    vec![
        EventType::ConfigChanged {
            json: "{\"fee_bps\":30}".into(),
        },
        EventType::FundsReceived {
            quote_hash: quote(1),
            quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
            chain_id: ChainId::BASE,
            token: text("USDC"),
            amount: amount(1_000_000),
            tx_ref: "0xfeed".into(),
        },
        EventType::TxSigned {
            quote_hash: quote(2),
            attempt: Attempt::new(1),
            chain_id: ChainId::ARBITRUM,
            tx_hash: TxHash::new([3; 32]),
            raw_tx: vec![0x02, 0xf8, 0x6b],
        },
        EventType::TxConfirmed {
            quote_hash: quote(4),
            attempt: Attempt::new(2),
            chain_id: ChainId::ETHEREUM,
            tx_hash: TxHash::new([5; 32]),
            block: BlockNumber::new(19_000_000),
        },
        EventType::TxFailed {
            quote_hash: quote(6),
            attempt: Attempt::new(3),
            reason: "reverted".into(),
        },
        EventType::PaidInStable {
            quote_hash: quote(7),
            chain_id: ChainId::BASE,
            amount: amount(999_999),
        },
        EventType::DecisionRequired {
            quote_hash: quote(8),
            reason: "slippage".into(),
        },
        EventType::DecisionMade {
            quote_hash: quote(9),
            choice: Choice::Refund,
        },
        EventType::RefundStarted {
            quote_hash: quote(10),
            reason: "timeout".into(),
        },
        EventType::Refunded {
            quote_hash: quote(11),
            chain_id: ChainId::POLYGON,
            token: text("USDT"),
            amount: amount(42),
            // empty text: length prefix must still be written
            to: text(""),
        },
        EventType::SwapDone {
            quote_hash: quote(12),
        },
        EventType::Frozen {
            quote_hash: quote(13),
            // multibyte utf8: length is bytes, not chars
            reason: "griefed \u{2603}".into(),
        },
        EventType::FeeAccrued {
            quote_hash: quote(14),
            amount: amount(7),
        },
        EventType::PocketFunded {
            chain_id: ChainId::new(10),
            // high word set: proves the u128 is 16 bytes, not a truncated u64
            amount: amount(1u128 << 70),
        },
        EventType::PocketReserved {
            quote_hash: quote(15),
            chain_id: ChainId::BASE,
            amount: amount(3),
        },
        EventType::PocketRebalanced {
            from_chain: ChainId::BASE,
            to_chain: ChainId::ARBITRUM,
            amount: amount(250),
            route: "cctp".into(),
        },
        EventType::PocketReleased {
            quote_hash: quote(16),
            chain_id: ChainId::BASE,
            amount: amount(150),
        },
        EventType::PocketSpent {
            quote_hash: quote(17),
            chain_id: ChainId::ARBITRUM,
            amount: amount(250),
        },
        EventType::RolesChanged {
            quoter: "aaaaa-aa".into(),
            watcher: "2vxsx-fae".into(),
        },
    ]
}

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/event_bytes_v1.txt");

fn tag_of(line: &str) -> &str {
    line.get(..4).unwrap_or("????")
}

fn swap_done(byte: u8) -> EventType {
    EventType::SwapDone {
        quote_hash: quote(byte),
    }
}

#[test]
fn choice_encodes_one_byte() {
    let q = EventType::DecisionMade {
        quote_hash: quote(0),
        choice: Choice::Requote,
    }
    .canonical_bytes();
    let r = EventType::DecisionMade {
        quote_hash: quote(0),
        choice: Choice::Refund,
    }
    .canonical_bytes();
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
    let got: Vec<String> = s.iter().map(|e| hex::encode(e.canonical_bytes())).collect();

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

/// A variant that fails to decode would trap post_upgrade and strand the canister on its
/// current wasm. This walks the same `Storable` impl the stable log uses, over every
/// variant, so the append-only storage rule is CI-enforced.
#[test]
fn every_variant_round_trips_through_storage() {
    let s = samples();
    assert_eq!(
        s.len(),
        EVENT_VARIANT_COUNT,
        "samples() must cover every variant"
    );
    for (i, payload) in (0u8..).zip(s) {
        let event = Event::seal(
            EventIndex::new(i.into()),
            Timestamp::from_nanos(1_700_000_000),
            EventHash::new([i; 32]),
            payload,
        );
        let back = Event::from_bytes(event.to_bytes());
        assert_eq!(back, event, "variant {i} does not survive stable storage");
    }
}

/// The two numberings of a variant are one assignment: its minicbor index is its
/// canonical tag.
#[test]
fn every_variant_stores_under_its_canonical_tag() {
    for payload in samples() {
        let cbor = minicbor::to_vec(&payload).unwrap();
        let mut d = minicbor::Decoder::new(&cbor);
        d.array().unwrap();
        let index = d.u16().unwrap();
        let tag = u16::from_be_bytes(payload.canonical_bytes()[..2].try_into().unwrap());
        assert_eq!(index, tag, "{payload:?}");
    }
}

#[test]
fn hash_changes_when_any_field_changes() {
    let at = |index: u64, nanos: u64, parent: u8, payload: EventType| {
        event_hash(
            EventIndex::new(index),
            Timestamp::from_nanos(nanos),
            &EventHash::new([parent; 32]),
            &payload,
        )
    };
    let a = at(1, 2, 0, swap_done(1));
    assert_ne!(a, at(2, 2, 0, swap_done(1)));
    assert_ne!(a, at(1, 3, 0, swap_done(1)));
    assert_ne!(a, at(1, 2, 9, swap_done(1)));
    assert_ne!(a, at(1, 2, 0, swap_done(2)));
}

#[test]
fn chain_links_and_detects_tampering() {
    let e0 = Event::seal(
        EventIndex::ZERO,
        Timestamp::from_nanos(100),
        EventHash::ZERO,
        EventType::PocketFunded {
            chain_id: ChainId::BASE,
            amount: amount(5),
        },
    );
    let e1 = Event::seal(
        EventIndex::new(1),
        Timestamp::from_nanos(200),
        e0.hash,
        swap_done(1),
    );
    assert!(chain_is_valid(&[e0.clone(), e1.clone()]));
    let mut bad = e1.clone();
    bad.payload = swap_done(2);
    assert!(!chain_is_valid(&[e0.clone(), bad]));
    let mut unlinked = e1;
    unlinked.parent_hash = EventHash::new([7; 32]);
    assert!(!chain_is_valid(&[e0, unlinked]));
}
