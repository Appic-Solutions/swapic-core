use crate::chain::ChainId;
use crate::config::Config;
use crate::events::{Event, EVENT_VARIANT_COUNT};
use crate::hash::{EventHash, QuoteHash};
use crate::ledger::LedgerMeta;
use crate::numeric::{Attempt, EventIndex, Timestamp, TokenAmount};
use crate::swap::{Pocket, Swap, SwapStatus, WaitingKey};
use ic_stable_structures::Storable;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Debug;

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/storage_v1.txt");

/// Asserts that golden bytes decode to a sample and encode back to themselves.
type DecodeCheck = Box<dyn Fn(&[u8])>;

/// One stored value, the bytes the current types write for it, and its decode check.
struct Sample {
    name: String,
    bytes: Vec<u8>,
    decodes_from: DecodeCheck,
}

fn sample<T: Storable + Debug + PartialEq + 'static>(name: impl Into<String>, value: T) -> Sample {
    let name = name.into();
    let label = name.clone();
    Sample {
        bytes: value.to_bytes().into_owned(),
        name,
        decodes_from: Box::new(move |golden| {
            let decoded = T::from_bytes(Cow::Borrowed(golden));
            assert_eq!(
                decoded, value,
                "{label}: the stored bytes now decode to another value"
            );
            assert_eq!(
                decoded.to_bytes().as_ref(),
                golden,
                "{label}: the decoded value encodes to other bytes"
            );
        }),
    }
}

/// Every type a stable structure holds, keys included, one sample per line of the golden
/// file: an `Event` per `EventType` variant in tag order, then two swaps (paid and
/// unpaid), a pocket, the ledger meta, a pending quote, the config, and the three key
/// types. Every field of a sample differs from its neighbours, so a field that moves to
/// another index decodes to a different value instead of passing unnoticed.
fn samples() -> Vec<Sample> {
    let events = crate::events::tests::samples();
    assert_eq!(
        events.len(),
        EVENT_VARIANT_COUNT,
        "samples() must cover every variant"
    );
    let mut all: Vec<Sample> = (0u8..)
        .zip(events)
        .map(|(i, payload)| {
            let name = format!("event {i}");
            let event = Event::seal(
                EventIndex::new(i.into()),
                Timestamp::from_nanos(1_700_000_000_000_000_000 + u64::from(i)),
                EventHash::new([i; 32]),
                payload,
            )
            .expect("every sample has a preimage");
            sample(name, event)
        })
        .collect();

    let paid = Swap {
        quote_bytes: vec![0xde, 0xad, 0xbe, 0xef],
        status: SwapStatus::WaitingForUser,
        last_attempt: Some(Attempt::new(3)),
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "USDC".parse().unwrap(),
        amount_in: TokenAmount::from(25_000_000_u32),
        amount_paid: Some(TokenAmount::from(24_990_000_u32)),
        waiting_since: Some(Timestamp::from_nanos(1_700_000_000_123_456_789)),
    };
    let unpaid = Swap {
        quote_bytes: vec![],
        status: SwapStatus::FundsReceived,
        last_attempt: None,
        open_attempt: Some(Attempt::FIRST),
        src_chain: ChainId::ARBITRUM,
        src_token: "USDT".parse().unwrap(),
        amount_in: TokenAmount::from(1u128 << 70),
        amount_paid: None,
        waiting_since: None,
    };
    let config = Config {
        platform_fee: crate::BasisPoints::new(10),
        rpc_urls: BTreeMap::from([(
            ChainId::ETHEREUM,
            "https://rpc.example/v2/key".parse().unwrap(),
        )]),
        vault_addresses: BTreeMap::from([(ChainId::BASE, "0xvault".parse().unwrap())]),
        ecdsa_key_name: "key_1".to_string(),
        ..Config::default()
    };
    all.extend([
        sample("swap paid", paid),
        sample("swap unpaid", unpaid),
        sample(
            "pocket",
            Pocket {
                // above u64::MAX, so the bignum form is pinned next to the plain one
                available: TokenAmount::from(1u128 << 70),
                reserved: TokenAmount::from(250_u32),
            },
        ),
        sample(
            "ledger meta",
            LedgerMeta {
                fees_accrued: TokenAmount::from(7_u32),
                next_event_index: EventIndex::new(19),
                last_event_hash: EventHash::new([0xab; 32]),
            },
        ),
        sample("pending quote", crate::quote::tests::fixed_quote()),
        sample("config", config),
        sample("quote hash key", QuoteHash::new([0x5a; 32])),
        sample("chain id key", ChainId::ARBITRUM),
        sample(
            "waiting key",
            WaitingKey {
                since: Timestamp::from_nanos(1_700_000_000_123_456_789),
                quote_hash: QuoteHash::new([0x5a; 32]),
            },
        ),
    ]);
    all
}

/// A stable log or map decodes nothing when it opens, so a wasm that breaks a stored
/// layout would upgrade cleanly and trap on every read after. This decodes the committed
/// bytes of every stored type with the types as they are now: a layout change fails here,
/// in CI, instead.
#[test]
fn every_stored_layout_decodes_from_the_golden_file() {
    let samples = samples();

    // regeneration is opt-in and never green, so a blessing is always a deliberate diff
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        let lines: Vec<String> = samples.iter().map(|s| hex::encode(&s.bytes)).collect();
        std::fs::write(GOLDEN, lines.join("\n") + "\n").unwrap();
        panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
    }

    let raw = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
        panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
    });
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines.len(),
        samples.len(),
        "golden has {} lines but samples() has {}: a new stored type appends a line, an old \
         line never changes",
        lines.len(),
        samples.len()
    );
    for (sample, line) in samples.iter().zip(lines) {
        let golden = hex::decode(line)
            .unwrap_or_else(|e| panic!("{}: golden line is not hex ({e})", sample.name));
        (sample.decodes_from)(&golden);
    }
}
