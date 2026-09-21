use crate::chain::ChainId;
use crate::chain_data::ChainReading;
use crate::config::{AuditChunk, Config, EvictionsPerSweep, RefundsPerSweep};
use crate::events::{Event, TxPurpose, EVENT_VARIANT_COUNT};
use crate::hash::{EventHash, QuoteHash, TxHash};
use crate::ledger::LedgerMeta;
use crate::numeric::{
    Attempt, BlockNumber, EventIndex, GasAmount, Nonce, Timestamp, TokenAmount, UnixSeconds, Wei,
    WeiPerGas,
};
use crate::quote::ExpiryKey;
use crate::swap::{Pocket, Swap, SwapStatus, WaitingKey};
use crate::tx::{NonceKey, OutboxEntry, OutboxStatus, UnsignedTx};
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
/// unpaid), a pocket, the ledger meta, a pending quote, two configs (every cap at its
/// default, and an interior cap set), one cached chain reading, one outbox entry, the five
/// key types, and one nonce waiting for its signature. Every field of a sample differs from
/// its neighbours, so a field that moves to another index decodes to a different value
/// instead of passing unnoticed.
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
    // every cap of the sample above holds its default, so its bytes stop where the caps
    // begin and pin only that one shape. This one sets an interior cap: the cap before it
    // is written out at its default and the cap after it is still omitted, which is the
    // other shape a stored config takes. The third, a null where a knob sits, is no golden
    // line, because a line must encode back to itself and an unset cap encodes as its
    // value; `config/tests.rs` decodes that one by hand.
    let mixed_caps = Config {
        max_refunds_per_sweep: RefundsPerSweep::DEFAULT,
        max_evictions_per_sweep: EvictionsPerSweep::new(1_234),
        audit_chunk_events: AuditChunk::DEFAULT,
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
        sample("config mixed caps", mixed_caps),
        sample("quote hash key", QuoteHash::new([0x5a; 32])),
        sample("chain id key", ChainId::ARBITRUM),
        sample(
            "waiting key",
            WaitingKey {
                since: Timestamp::from_nanos(1_700_000_000_123_456_789),
                quote_hash: QuoteHash::new([0x5a; 32]),
            },
        ),
        sample(
            "expiry key",
            ExpiryKey {
                expires_at: UnixSeconds::new(1_800_000_000),
                quote_hash: QuoteHash::new([0x5b; 32]),
            },
        ),
        sample(
            "chain data",
            ChainReading {
                block: BlockNumber::new(19_000_000),
                base_fee: WeiPerGas::from(1_000_000_000_u64),
                priority_fee: WeiPerGas::from(100_000_000_u64),
            }
            .pushed_at(Timestamp::from_nanos(1_700_000_000_123_456_789)),
        ),
        sample(
            "outbox entry",
            OutboxEntry {
                purpose: TxPurpose::Burn(QuoteHash::new([0x5c; 32])),
                chain_id: ChainId::BASE,
                nonce: Nonce::new(7),
                attempt: Some(Attempt::new(2)),
                hashes: vec![TxHash::new([0x5d; 32]), TxHash::new([0x5e; 32])],
                raw_tx: vec![0x02, 0xf8, 0x6b],
                max_fee: WeiPerGas::from(2_000_000_000_u64),
                max_priority_fee: WeiPerGas::from(100_000_000_u64),
                status: OutboxStatus::Sent,
                created_at: Timestamp::from_nanos(1_700_000_000_123_456_789),
                last_sent_at: Some(Timestamp::from_nanos(1_700_000_001_000_000_000)),
                to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![0xde, 0xad, 0xbe, 0xef],
                gas_limit: GasAmount::from(120_000_u32),
            },
        ),
        sample(
            "nonce key",
            NonceKey {
                chain_id: ChainId::ARBITRUM,
                nonce: Nonce::new(9),
            },
        ),
        sample(
            "unsigned nonce",
            UnsignedTx {
                purpose: TxPurpose::Refund(QuoteHash::new([0x5f; 32])),
                created_at: Timestamp::from_nanos(1_700_000_002_000_000_000),
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
