use crate::chain::ChainId;
use crate::chain_data::ChainReading;
use crate::config::{
    AuditChunk, ChainTable, Config, EvictionsPerSweep, RefundsPerSweep, MIN_ETHEREUM_CONFIRMATIONS,
};
use crate::events::{Event, TxPurpose, EVENT_VARIANT_COUNT};
use crate::hash::{EventHash, QuoteHash, TxHash};
use crate::ledger::LedgerMeta;
use crate::numeric::{
    Attempt, BlockDepth, BlockNumber, EventIndex, GasAmount, Nonce, Timestamp, TokenAmount,
    UnixSeconds, Wei, WeiPerGas,
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
/// file, in the order the lines were added: an `Event` per `EventType` variant in tag
/// order, two swaps (paid and unpaid), a pocket, the ledger meta, a pending quote, two
/// configs (every cap at its default, and an interior cap set), one cached chain reading,
/// one outbox entry, the five key types, one nonce waiting for its signature, a cancel
/// that has been out on the network, a sanctioned address as the sanctions set keys it, an
/// in-flight marker, an attestation, a swap whose latest leg is known, an Eco intent, a
/// swap with the hash its latest attempt confirmed as, an outbox entry the provider
/// refused, a swap with the amount its payout was signed for, a pending quote entry with
/// the height it was registered at, a config at the depths a deploy gets, two swaps with
/// the fees their record holds, a pending quote entry with the deadlines it was registered
/// under, a config at the depths a deploy gets once a swap on Ethereum is acted on at 1
/// block, a config with crediting depths of its own, a books line with the block its fee
/// was read at, a chain's books with a fee no check has read yet, an outbox entry kept
/// closed until the crediting depth, and one with the outcome it closed on.
///
/// Every field of a sample differs from its neighbours, so a field that moves to another
/// index decodes to a different value instead of passing unnoticed. The file is append
/// only: a new stored type adds a line at the end, and a default that moves adds a line of
/// its own rather than changing the line above it.
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
        // absent, so this sample's bytes stay exactly the ones the golden already pins; the
        // swaps appended at the end of `samples()` are the ones that pin the later fields
        last_leg: None,
        last_outcome: None,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
        burn_max_fee: None,
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
        last_leg: None,
        last_outcome: None,
        last_tx_hash: None,
        paid_out: None,
        fee_accrued: None,
        burn_max_fee: None,
    };
    // the depths the default held when this line was written. A default that moves changes
    // what every config encodes, and a golden line never changes, so the two lines below
    // spell the old table out and the new default is pinned by a line of its own at the
    // end of `samples()`.
    let depths_before_the_mainnet_floor = BTreeMap::from([
        (ChainId::ETHEREUM, BlockDepth::new(1)),
        (ChainId::BASE, BlockDepth::new(1)),
        (ChainId::BSC, BlockDepth::new(1)),
        (ChainId::POLYGON, BlockDepth::new(6)),
        (ChainId::ARBITRUM, BlockDepth::new(1)),
    ]);
    let config = Config {
        platform_fee: crate::BasisPoints::new(10),
        rpc_urls: BTreeMap::from([(
            ChainId::ETHEREUM,
            "https://rpc.example/v2/key".parse().unwrap(),
        )]),
        vault_addresses: BTreeMap::from([(ChainId::BASE, "0xvault".parse().unwrap())]),
        ecdsa_key_name: "key_1".to_string(),
        confirmations: depths_before_the_mainnet_floor.clone(),
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
        confirmations: depths_before_the_mainnet_floor,
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
                pool_paused_by: None,
                pool_paused_at: None,
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
                claim_until: UnixSeconds::new(1_800_000_000),
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
                // absent, so this sample's bytes stay exactly the ones the golden already
                // pins: a record stops at its last field that carries a value. The entry
                // appended at the end of `samples()` is the one that pins this field.
                first_sent_at: None,
                to: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![0xde, 0xad, 0xbe, 0xef],
                gas_limit: GasAmount::from(120_000_u32),
                refusal: None,
                closed_at: None,
                closed_success: None,
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
                action: None,
            },
        ),
        sample(
            "outbox entry on the network",
            OutboxEntry {
                purpose: TxPurpose::Cancel(ChainId::POLYGON),
                chain_id: ChainId::POLYGON,
                nonce: Nonce::new(11),
                attempt: None,
                hashes: vec![TxHash::new([0x60; 32])],
                raw_tx: vec![0x02, 0xf8, 0x6c],
                max_fee: WeiPerGas::from(3_000_000_000_u64),
                max_priority_fee: WeiPerGas::from(200_000_000_u64),
                status: OutboxStatus::Sent,
                created_at: Timestamp::from_nanos(1_700_000_003_000_000_000),
                last_sent_at: Some(Timestamp::from_nanos(1_700_000_005_000_000_000)),
                first_sent_at: Some(Timestamp::from_nanos(1_700_000_004_000_000_000)),
                to: "0x4200000000000000000000000000000000000006"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![],
                gas_limit: GasAmount::from(21_000_u32),
                // absent, so this sample's bytes stay exactly the ones the golden pins;
                // the entry appended below is the one that pins the field
                refusal: None,
                closed_at: None,
                closed_success: None,
            },
        ),
        sample(
            "sanctioned address key",
            "0x1111111111111111111111111111111111111111"
                .parse::<crate::Address>()
                .unwrap(),
        ),
        sample(
            "in-flight marker",
            crate::InFlight {
                kind: crate::InFlightKind::Pull,
                since: Timestamp::from_nanos(1_700_000_006_000_000_000),
            },
        ),
        sample(
            "attestation",
            crate::Attestation::new(
                vec![0x61; 40],
                vec![0x62; 65],
                Timestamp::from_nanos(1_700_000_007_000_000_000),
            )
            .unwrap(),
        ),
        sample(
            "swap with its latest leg known",
            Swap {
                quote_bytes: vec![0xca, 0xfe],
                status: SwapStatus::Executing,
                last_attempt: Some(Attempt::new(2)),
                open_attempt: None,
                src_chain: ChainId::ETHEREUM,
                src_token: "0x1".parse().unwrap(),
                amount_in: TokenAmount::from(5_000_000_u32),
                amount_paid: None,
                waiting_since: None,
                last_leg: Some(crate::Leg::Mint),
                last_outcome: Some(crate::Outcome::Failed),
                // absent, so this sample's bytes stay exactly the ones the golden pins;
                // the swap appended below is the one that pins the later fields
                last_tx_hash: None,
                paid_out: None,
                fee_accrued: None,
                burn_max_fee: None,
            },
        ),
        sample(
            "eco intent",
            crate::EcoIntent::new(
                ChainId::BASE,
                vec![0x63, 0x64, 0x65],
                UnixSeconds::new(1_788_357_691),
                "0xeC00008537c1F26E739486BCFCC818d81234d5aD"
                    .parse()
                    .unwrap(),
            )
            .unwrap(),
        ),
        sample(
            "swap with the hash its latest attempt confirmed as",
            Swap {
                quote_bytes: vec![0xca, 0xfe, 0x01],
                status: SwapStatus::Executing,
                last_attempt: Some(Attempt::new(1)),
                open_attempt: None,
                src_chain: ChainId::BASE,
                src_token: "0x2".parse().unwrap(),
                amount_in: TokenAmount::from(6_000_000_u32),
                amount_paid: None,
                waiting_since: None,
                last_leg: Some(crate::Leg::Burn),
                last_outcome: Some(crate::Outcome::Confirmed),
                last_tx_hash: Some(TxHash::new([0x66; 32])),
                // absent, so this sample's bytes stay exactly the ones the golden pins;
                // the swap appended below is the one that pins the field
                paid_out: None,
                fee_accrued: None,
                burn_max_fee: None,
            },
        ),
        sample(
            "outbox entry the provider refused",
            OutboxEntry {
                purpose: TxPurpose::Payout(QuoteHash::new([0x67; 32])),
                chain_id: ChainId::BASE,
                nonce: Nonce::new(12),
                attempt: Some(Attempt::new(3)),
                hashes: vec![TxHash::new([0x68; 32])],
                raw_tx: vec![0x02, 0xf8, 0x6d],
                max_fee: WeiPerGas::from(4_000_000_000_u64),
                max_priority_fee: WeiPerGas::from(300_000_000_u64),
                status: OutboxStatus::Sent,
                created_at: Timestamp::from_nanos(1_700_000_008_000_000_000),
                last_sent_at: Some(Timestamp::from_nanos(1_700_000_009_000_000_000)),
                first_sent_at: Some(Timestamp::from_nanos(1_700_000_009_000_000_000)),
                to: "0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![0x40, 0x10, 0x47, 0x63],
                gas_limit: GasAmount::from(120_000_u32),
                refusal: Some(crate::tx::Refusal::new("transaction underpriced")),
                closed_at: None,
                closed_success: None,
            },
        ),
        sample(
            "swap with the amount its payout was signed for",
            Swap {
                quote_bytes: vec![0xca, 0xfe, 0x02],
                status: SwapStatus::Delivering,
                last_attempt: Some(Attempt::new(3)),
                open_attempt: None,
                src_chain: ChainId::ARBITRUM,
                src_token: "0x3".parse().unwrap(),
                amount_in: TokenAmount::from(7_000_000_u32),
                amount_paid: Some(TokenAmount::from(6_999_000_u32)),
                waiting_since: None,
                last_leg: Some(crate::Leg::Payout),
                last_outcome: Some(crate::Outcome::Confirmed),
                last_tx_hash: Some(TxHash::new([0x69; 32])),
                paid_out: Some(TokenAmount::from(6_978_003_u32)),
                fee_accrued: None,
                burn_max_fee: None,
            },
        ),
        // the entry the pending store holds now: the quote with the height the watcher had
        // reported when it was registered. The `pending quote` line above is the quote
        // alone, which is what the entry holds inside it and what the store held before.
        sample(
            "pending quote entry",
            crate::PendingQuote {
                quote: crate::quote::tests::fixed_quote(),
                registered_at: Some(BlockNumber::new(19_000_123)),
                // absent, so this sample's bytes stay exactly the ones the golden pins; the
                // entry appended at the end of `samples()` is the one that pins the field
                deadlines: None,
            },
        ),
        // the depths a deploy got, mainnet's floor among them: the two config lines above
        // keep the table this one replaced, so neither of them moves. Spelled out since
        // P10c moved Ethereum's swap depth to 1, so this line does not move either
        sample(
            "config at the depths a deploy gets",
            Config {
                ecdsa_key_name: "key_1".to_string(),
                confirmations: BTreeMap::from([
                    (ChainId::ETHEREUM, MIN_ETHEREUM_CONFIRMATIONS),
                    (ChainId::BASE, BlockDepth::new(1)),
                    (ChainId::BSC, BlockDepth::new(1)),
                    (ChainId::POLYGON, BlockDepth::new(6)),
                    (ChainId::ARBITRUM, BlockDepth::new(1)),
                ]),
                ..Config::default()
            },
        ),
        // the swap samples above leave the fee absent, so their bytes stay the ones the
        // golden pins; this one pins it
        sample(
            "swap with the fee the log accrued for it",
            Swap {
                quote_bytes: vec![0xca, 0xfe, 0x03],
                status: SwapStatus::Delivering,
                last_attempt: Some(Attempt::new(3)),
                open_attempt: None,
                src_chain: ChainId::ARBITRUM,
                src_token: "0x4".parse().unwrap(),
                amount_in: TokenAmount::from(8_000_000_u32),
                amount_paid: Some(TokenAmount::from(7_999_000_u32)),
                waiting_since: None,
                last_leg: Some(crate::Leg::Payout),
                last_outcome: Some(crate::Outcome::Confirmed),
                last_tx_hash: Some(TxHash::new([0x6a; 32])),
                paid_out: Some(TokenAmount::from(7_975_003_u32)),
                fee_accrued: Some(TokenAmount::from(23_997_u32)),
                burn_max_fee: None,
            },
        ),
        // the swap samples above leave the burn's fee absent; this one pins it
        sample(
            "swap with the fee its burn offered Circle",
            Swap {
                quote_bytes: vec![0xca, 0xfe, 0x04],
                status: SwapStatus::Executing,
                last_attempt: Some(Attempt::new(1)),
                open_attempt: None,
                src_chain: ChainId::BASE,
                src_token: "0x5".parse().unwrap(),
                amount_in: TokenAmount::from(25_000_000_u32),
                amount_paid: None,
                waiting_since: None,
                last_leg: Some(crate::Leg::Burn),
                last_outcome: Some(crate::Outcome::Confirmed),
                last_tx_hash: Some(TxHash::new([0x6b; 32])),
                paid_out: None,
                fee_accrued: None,
                burn_max_fee: Some(TokenAmount::from(12_500_u32)),
            },
        ),
        // the pending entry with the deadlines it was registered under: the entry above
        // leaves them absent, so its bytes stay the ones the golden pins
        sample(
            "pending quote entry with the deadlines it was registered under",
            crate::PendingQuote {
                quote: crate::quote::tests::fixed_quote(),
                registered_at: Some(BlockNumber::new(19_000_456)),
                deadlines: Some(crate::QuoteDeadlines {
                    deposit_until: UnixSeconds::new(1_800_000_120),
                    claim_until: UnixSeconds::new(1_800_003_720),
                }),
            },
        ),
        // the depths a deploy gets since P10c: a swap on Ethereum is acted on at 1 block,
        // and with no crediting depth set the field is absent, as in every line above. The
        // table is spelled out, so a later default cannot move this line
        sample(
            "config at the depths a deploy gets with ethereum swaps at 1 block",
            Config {
                ecdsa_key_name: "key_1".to_string(),
                confirmations: depths_with_ethereum_swaps_at_one_block(),
                ..Config::default()
            },
        ),
        // the config lines above leave the crediting depths absent; this one pins them
        sample(
            "config with crediting depths of its own",
            Config {
                ecdsa_key_name: "key_1".to_string(),
                confirmations: depths_with_ethereum_swaps_at_one_block(),
                credit_confirmations: ChainTable(BTreeMap::from([
                    (ChainId::ETHEREUM, BlockDepth::new(14)),
                    (ChainId::BASE, BlockDepth::new(2)),
                ])),
                ..Config::default()
            },
        ),
        // the books line above (`event 25`) leaves the fee's block absent; this one pins it
        sample(
            "books line with the block its fee was read at",
            Event::seal(
                EventIndex::new(1_000),
                Timestamp::from_nanos(1_800_000_000_000_000_000),
                EventHash::new([0x4b; 32]),
                crate::events::EventType::Books {
                    chain_id: ChainId::ETHEREUM,
                    step: crate::books::BookStep::FeeEarned {
                        tx_hash: TxHash::new([0x4c; 32]),
                        amount: TokenAmount::from(157_740_u32),
                        block: Some(BlockNumber::new(23_600_001)),
                    },
                },
            )
            .expect("every sample has a preimage"),
        ),
        // a chain's books, with a fee no balance check has read at yet
        sample(
            "books with a fee no check has read yet",
            crate::books::Books {
                fee_income: TokenAmount::from(900_001_u32),
                unattributed: TokenAmount::from(2_u8),
                pool: TokenAmount::from(1u128 << 70),
                last_check: Some(crate::books::BalanceCheck {
                    block: BlockNumber::new(23_599_990),
                    observed: TokenAmount::from(742_259_u32),
                    expected: TokenAmount::from(742_261_u32),
                }),
                checked_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_001)),
                failed_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_002)),
                unseen_fees: crate::books::UnseenFees(vec![crate::books::BookedFee {
                    block: BlockNumber::new(23_600_001),
                    amount: TokenAmount::from(157_740_u32),
                }]),
            },
        ),
        // the outbox entries above are open; this one pins the block its attempt closed at
        sample(
            "outbox entry kept closed until the crediting depth",
            OutboxEntry {
                purpose: TxPurpose::Relay(QuoteHash::new([0x4d; 32])),
                chain_id: ChainId::ETHEREUM,
                nonce: Nonce::new(41),
                attempt: None,
                hashes: vec![TxHash::new([0x4e; 32]), TxHash::new([0x4f; 32])],
                raw_tx: vec![0x02, 0xf8, 0x71],
                max_fee: WeiPerGas::from(6_000_000_000_u64),
                max_priority_fee: WeiPerGas::from(500_000_000_u64),
                status: OutboxStatus::Sent,
                created_at: Timestamp::from_nanos(1_800_000_000_000_000_003),
                last_sent_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_005)),
                first_sent_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_004)),
                to: "0x2222222222222222222222222222222222222222"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![0x3b, 0x8c, 0x2c, 0x51],
                gas_limit: GasAmount::from(392_732_u32),
                refusal: None,
                closed_at: Some(BlockNumber::new(23_600_002)),
                closed_success: None,
            },
        ),
        // the kept entry above leaves its outcome absent; this one pins it
        sample(
            "outbox entry kept closed with the outcome it closed on",
            OutboxEntry {
                purpose: TxPurpose::Finish(QuoteHash::new([0x50; 32])),
                chain_id: ChainId::ETHEREUM,
                nonce: Nonce::new(42),
                attempt: None,
                hashes: vec![TxHash::new([0x51; 32])],
                raw_tx: vec![0x02, 0xf8, 0x72],
                max_fee: WeiPerGas::from(7_000_000_000_u64),
                max_priority_fee: WeiPerGas::from(600_000_000_u64),
                status: OutboxStatus::Queued,
                created_at: Timestamp::from_nanos(1_800_000_000_000_000_006),
                last_sent_at: None,
                first_sent_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_007)),
                to: "0x3333333333333333333333333333333333333333"
                    .parse()
                    .unwrap(),
                value: Wei::ZERO,
                data: vec![0x6e, 0x5c, 0x37, 0x6a],
                gas_limit: GasAmount::from(668_092_u32),
                refusal: None,
                closed_at: Some(BlockNumber::new(23_600_003)),
                closed_success: Some(false),
            },
        ),
    ]);
    all.extend(pool_samples());
    all.extend(payout_samples());
    all.extend(route_samples());
    all.extend(pool_tx_samples());
    all.extend(icp_swap_samples());
    all.extend(queue_samples());
    all.extend(orphan_samples());
    all
}

/// The orphans (P11b Task 7): one held, one refunding, and each of their steps.
fn orphan_samples() -> Vec<Sample> {
    use crate::pool::{CreditKey, Orphan, OrphanStatus, PoolStep};
    let key = CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0x81; 32]),
        log_index: 801,
    };
    let held = Orphan {
        amount: TokenAmount::from(802_000_000_u64),
        block: BlockNumber::new(37_000_803),
        payer: Some(crate::evm::EvmAddress::new([0x84; 20])),
        why: "no deposit is registered under the ref".to_string(),
        status: OrphanStatus::Held,
    };
    let refunding = Orphan {
        payer: None,
        status: OrphanStatus::Refunding {
            to: crate::evm::EvmAddress::new([0x85; 20]),
            sent: TokenAmount::from(806_000_000_u64),
        },
        ..held.clone()
    };
    let steps = [
        ("pool orphan booked", PoolStep::OrphanBooked { key }),
        (
            "pool orphan refund signed",
            PoolStep::OrphanRefundSigned {
                key,
                to: crate::evm::EvmAddress::new([0x85; 20]),
                sent: TokenAmount::from(806_000_000_u64),
                chain_id: ChainId::BASE,
                nonce: Nonce::new(807),
                tx_hash: TxHash::new([0x88; 32]),
                raw_tx: vec![0x02, 0x89],
            },
        ),
        (
            "pool orphan refund closed",
            PoolStep::OrphanRefundClosed {
                key,
                success: true,
            },
        ),
    ];
    let mut out = vec![
        sample("pool orphan held", held),
        sample("pool orphan refunding", refunding),
    ];
    out.extend((0_u64..).zip(steps).map(|(i, (name, step))| {
        sample(
            name,
            Event::seal(
                EventIndex::new(8_000 + i),
                Timestamp::from_nanos(1_800_000_000_000_000_810 + i),
                EventHash::new([0xcb; 32]),
                crate::events::EventType::Pool { step },
            )
            .expect("every sample has a preimage"),
        )
    }));
    out
}

/// The queue, the deadlines and the Eco refund (P11b Task 6): a payout whose ICP hold was
/// released, a relisting, and a refund credited back.
fn queue_samples() -> Vec<Sample> {
    use crate::pool::payout::{
        EvmPayout, Flag, IcpSwapTerms, PayoutKind, PayoutStatus, PayoutTerms, Undo,
    };
    use crate::pool::{CreditKey, IcpAccount, PoolStep};
    use candid::Principal;

    let account = IcpAccount {
        owner: Principal::from_slice(&[0x71; 10]),
        subaccount: None,
    };
    let mut payout = EvmPayout::new(
        601,
        PayoutKind::IcpSwap(IcpSwapTerms {
            token_in: Principal::from_slice(&[0x72; 10]),
            amount_in: TokenAmount::from(602_000_000_u64),
            from: account,
            min_usdc: TokenAmount::from(602_000_000_u64),
            premium: TokenAmount::from(603_000_u64),
            network_fee: TokenAmount::from(604_000_u64),
            refund_to: account,
        }),
        PayoutTerms {
            dst_chain: ChainId::ARBITRUM,
            recipient: crate::evm::EvmAddress::new([0x73; 20]),
            token_out: crate::evm::EvmAddress::new([0x74; 20]),
            min_out: TokenAmount::from(605_u32),
            deadline: Timestamp::from_nanos(1_800_000_000_000_000_606),
        },
        Undo::Refund { to: account },
        Timestamp::from_nanos(1_800_000_000_000_000_607),
    );
    payout.status = PayoutStatus::Paid;
    payout.received = Some(TokenAmount::from(602_000_000_u64));
    payout.intent = Some([0x75; 32]);
    payout.icp_released = Flag(true);
    let steps = [
        ("pool payout relisted", PoolStep::PayoutRelisted { id: 608 }),
        (
            "pool payout refunded by Eco",
            PoolStep::PayoutRefunded {
                id: 601,
                credit: CreditKey {
                    chain: ChainId::BASE,
                    tx: TxHash::new([0x76; 32]),
                    log_index: 609,
                },
                amount: TokenAmount::from(610_000_000_u64),
                block: BlockNumber::new(37_000_611),
            },
        ),
    ];
    let mut out = vec![sample("pool payout whose ICP hold was released", payout)];
    out.extend((0_u64..).zip(steps).map(|(i, (name, step))| {
        sample(
            name,
            Event::seal(
                EventIndex::new(7_000 + i),
                Timestamp::from_nanos(1_800_000_000_000_000_612 + i),
                EventHash::new([0xca; 32]),
                crate::events::EventType::Pool { step },
            )
            .expect("every sample has a preimage"),
        )
    }));
    out
}

/// An ICP swap's legs into ckUSDC (P11b Task 3): a payout with its legs and the instant its
/// undo began, the pull, and a stop for a human.
fn icp_swap_samples() -> Vec<Sample> {
    use crate::pool::payout::{
        EvmPayout, IcpSwapTerms, PayoutKind, PayoutStatus, PayoutTerms, Undo,
    };
    use crate::pool::{
        IcpAccount, PayoutAttempt, PayoutPath, PayoutProgress, PoolStep, PrincipalBytes, SwapLeg,
    };
    use candid::Principal;

    let principal = |byte: u8| Principal::from_slice(&[byte; 10]);
    let swap = IcpSwapTerms {
        token_in: principal(0x61),
        amount_in: TokenAmount::from(5_000_000_u64),
        from: IcpAccount {
            owner: principal(0x62),
            subaccount: None,
        },
        min_usdc: TokenAmount::from(2_000_000_u64),
        premium: TokenAmount::from(40_000_u64),
        network_fee: TokenAmount::from(50_000_u64),
        refund_to: IcpAccount {
            owner: principal(0x63),
            subaccount: Some([0x64; 32]),
        },
    };
    let attempt = PayoutAttempt {
        created_at: Timestamp::from_nanos(1_800_000_000_000_000_501),
        memo: [0x65; 32],
        path: PayoutPath::IcpswapVia {
            first: principal(0x66),
            second: principal(0x67),
            icp: principal(0x68),
        },
        amount: Some(TokenAmount::from(5_000_000_u64)),
        token: Some(PrincipalBytes(principal(0x61))),
    };
    let mut payout = EvmPayout::new(
        502,
        PayoutKind::IcpSwap(swap),
        PayoutTerms {
            dst_chain: ChainId::BASE,
            recipient: crate::evm::EvmAddress::new([0x69; 20]),
            token_out: crate::evm::EvmAddress::new([0x6a; 20]),
            min_out: TokenAmount::from(503_u32),
            deadline: Timestamp::from_nanos(1_800_000_000_000_000_504),
        },
        Undo::Refund { to: swap.refund_to },
        Timestamp::from_nanos(1_800_000_000_000_000_505),
    );
    payout.status = PayoutStatus::Undoing("below min_usdc".to_string());
    payout.icp_block = Some(506);
    payout.undoing_at = Some(Timestamp::from_nanos(1_800_000_000_000_000_507));
    payout.legs = Some(PayoutProgress {
        attempt: Some(attempt),
        swap: SwapLeg {
            declined: 0,
            begun: true,
            out: Some(TokenAmount::from(508_u32)),
            send: Some(TokenAmount::from(507_u32)),
            approvals: Some(1),
            undone: None,
            before: None,
            withdrawn: None,
            amount_in: Some(TokenAmount::from(4_998_000_u64)),
        },
        ..PayoutProgress::default()
    });
    let steps = [
        (
            "pool payout pulled",
            PoolStep::PayoutPulled {
                id: 502,
                block: 509,
                attempt: Some(attempt),
            },
        ),
        (
            "pool payout stopped",
            PoolStep::PayoutStopped {
                id: 510,
                why: "ALERT: for a human".to_string(),
            },
        ),
    ];
    let mut out = vec![sample("pool payout with its legs into ckUSDC", payout)];
    out.extend((0_u64..).zip(steps).map(|(i, (name, step))| {
        sample(
            name,
            Event::seal(
                EventIndex::new(6_000 + i),
                Timestamp::from_nanos(1_800_000_000_000_000_511 + i),
                EventHash::new([0xc9; 32]),
                crate::events::EventType::Pool { step },
            )
            .expect("every sample has a preimage"),
        )
    }));
    out
}

/// The pool's own transactions (P11b Task 2): a chain with the pool's gas rows, a
/// transaction created for each pool purpose, and the approval's signed record.
fn pool_tx_samples() -> Vec<Sample> {
    use crate::config::{EvmChain, GasAction, PerAction};
    use crate::evm::EvmAddress;
    use crate::pool::PoolStep;

    let address = |byte: u8| EvmAddress::new([byte; 20]);
    let rows = |base: u64| PerAction {
        relay_cctp: base,
        relay_eco: base + 1,
        relay_local: base + 2,
        complete_swap: base + 3,
        complete_ck: base + 4,
        relay_deliver: base + 5,
        release: base + 6,
        pool_payout: Some(base + 7),
        pool_transfer: Some(base + 8),
        pool_approve: Some(base + 9),
    };
    let chain = EvmChain {
        swapic: address(0x51),
        runner: address(0x52),
        cctp_receiver: address(0x53),
        usdc: address(0x54),
        token_messenger: address(0x55),
        message_transmitter: address(0x56),
        gas_limit: {
            let r = rows(400_000);
            PerAction {
                relay_cctp: GasAmount::from(r.relay_cctp),
                relay_eco: GasAmount::from(r.relay_eco),
                relay_local: GasAmount::from(r.relay_local),
                complete_swap: GasAmount::from(r.complete_swap),
                complete_ck: GasAmount::from(r.complete_ck),
                relay_deliver: GasAmount::from(r.relay_deliver),
                release: GasAmount::from(r.release),
                pool_payout: r.pool_payout.map(GasAmount::from),
                pool_transfer: r.pool_transfer.map(GasAmount::from),
                pool_approve: r.pool_approve.map(GasAmount::from),
            }
        },
        gas_budget: {
            let r = rows(1_000_000_000_000_000);
            PerAction {
                relay_cctp: Wei::from(r.relay_cctp),
                relay_eco: Wei::from(r.relay_eco),
                relay_local: Wei::from(r.relay_local),
                complete_swap: Wei::from(r.complete_swap),
                complete_ck: Wei::from(r.complete_ck),
                relay_deliver: Wei::from(r.relay_deliver),
                release: Wei::from(r.release),
                pool_payout: r.pool_payout.map(Wei::from),
                pool_transfer: r.pool_transfer.map(Wei::from),
                pool_approve: r.pool_approve.map(Wei::from),
            }
        },
        min_network_fee: {
            let r = rows(50_000);
            PerAction {
                relay_cctp: TokenAmount::from(r.relay_cctp),
                relay_eco: TokenAmount::from(r.relay_eco),
                relay_local: TokenAmount::from(r.relay_local),
                complete_swap: TokenAmount::from(r.complete_swap),
                complete_ck: TokenAmount::from(r.complete_ck),
                relay_deliver: TokenAmount::from(r.relay_deliver),
                release: TokenAmount::from(r.release),
                pool_payout: r.pool_payout.map(TokenAmount::from),
                pool_transfer: r.pool_transfer.map(TokenAmount::from),
                pool_approve: r.pool_approve.map(TokenAmount::from),
            }
        },
    };
    let config = Config {
        ecdsa_key_name: "key_1".to_string(),
        confirmations: depths_with_ethereum_swaps_at_one_block(),
        evm_chains: ChainTable(BTreeMap::from([(ChainId::BASE, chain)])),
        pool_chain: Some(ChainId::BASE),
        ..Config::default()
    };
    let created = |i: u64, purpose: TxPurpose, action: GasAction| {
        Event::seal(
            EventIndex::new(5_000 + i),
            Timestamp::from_nanos(1_800_000_000_000_000_400 + i),
            EventHash::new([0xc8; 32]),
            crate::events::EventType::TxCreated {
                purpose,
                chain_id: ChainId::BASE,
                nonce: Nonce::new(401 + i),
                to: address(0x57),
                value: Wei::ZERO,
                data: vec![0x58; 36],
                gas_limit: GasAmount::from(402_000_u64),
                max_fee: WeiPerGas::new(403_000_000),
                max_priority_fee: WeiPerGas::new(404_000),
                action: Some(action),
            },
        )
        .expect("every sample has a preimage")
    };
    vec![
        sample("config with the pool's gas rows", config),
        sample(
            "pool payout transaction created",
            created(0, TxPurpose::PoolPayout(405), GasAction::PoolPayout),
        ),
        sample(
            "pool transfer transaction created",
            created(
                1,
                TxPurpose::PoolTransfer([0x59; 32]),
                GasAction::PoolTransfer,
            ),
        ),
        sample(
            "pool approval transaction created",
            created(2, TxPurpose::PoolApprove, GasAction::PoolApprove),
        ),
        sample(
            "pool approval signed",
            Event::seal(
                EventIndex::new(5_003),
                Timestamp::from_nanos(1_800_000_000_000_000_406),
                EventHash::new([0xc8; 32]),
                crate::events::EventType::Pool {
                    step: PoolStep::ApproveSigned {
                        chain_id: ChainId::BASE,
                        nonce: Nonce::new(403),
                        tx_hash: TxHash::new([0x5a; 32]),
                        raw_tx: vec![0x02, 0x5b],
                    },
                },
            )
            .expect("every sample has a preimage"),
        ),
    ]
}

/// The ICP routes' stored types (P11b Task 1b): settings with their routes, a path through
/// ICP, a payout with its second leg and its way back, and every swap step naming its leg.
fn route_samples() -> Vec<Sample> {
    use crate::hash::PoolRef;
    use crate::pool::{
        IcpRoute, IcpRouteVia, IcpRoutes, LedgerFee, PayoutAttempt, PayoutPath, PayoutProgress,
        PoolHeld, PoolLedger, PoolSettings, PoolStep, PrincipalBytes, SwapLeg, LEG_BACK,
        LEG_SECOND,
    };
    use candid::Principal;

    let principal = |byte: u8| Principal::from_slice(&[byte; 10]);
    let routes = IcpRoutes {
        icp_ckusdc_pool: Some(PrincipalBytes(principal(0x31))),
        icp_ledger: Some(PrincipalBytes(principal(0x32))),
        routes: vec![
            IcpRoute {
                ledger: principal(0x32),
                via: IcpRouteVia::Direct(principal(0x31)),
            },
            IcpRoute {
                ledger: principal(0x33),
                via: IcpRouteVia::Icp(principal(0x34)),
            },
        ],
        ledger_fees: vec![LedgerFee {
            ledger: principal(0x33),
            fee: TokenAmount::from(0x35_u8),
        }],
    };
    let ledger = PoolLedger {
        settings: Some(PoolSettings {
            routes: Some(routes.clone()),
            ..PoolSettings::default()
        }),
        ..PoolLedger::default()
    };
    let path = PayoutPath::IcpswapVia {
        first: principal(0x31),
        second: principal(0x34),
        icp: principal(0x32),
    };
    let leg = |byte: u8| SwapLeg {
        declined: 1,
        begun: true,
        out: Some(TokenAmount::from(byte)),
        send: Some(TokenAmount::from(byte - 1)),
        approvals: Some(1),
        undone: None,
        before: Some(PoolHeld {
            ckusdc: TokenAmount::from(byte - 2),
            token: TokenAmount::from(byte - 3),
        }),
        withdrawn: None,
        amount_in: Some(TokenAmount::from(byte - 4)),
    };
    let progress = PayoutProgress {
        attempt: Some(PayoutAttempt {
            created_at: Timestamp::from_nanos(1_800_000_000_000_000_301),
            memo: [0x36; 32],
            path,
            amount: Some(TokenAmount::from(302_u32)),
            token: Some(PrincipalBytes(principal(0x37))),
        }),
        swap: leg(0x40),
        failures: 0,
        fell_back: Some("the second leg could not pay".to_string()),
        failed: None,
        second: Some(leg(0x50)),
        back: Some(leg(0x60)),
    };
    let r#ref = PoolRef::new([0x38; 32]);
    let steps = [
        (
            "pool routes set",
            PoolStep::IcpRoutesSet {
                routes: routes.clone(),
            },
        ),
        (
            "pool payout started through ICP",
            PoolStep::PayoutStarted {
                r#ref,
                attempt: progress.attempt.unwrap(),
            },
        ),
        (
            "pool swap declined on its second leg",
            PoolStep::SwapDeclined {
                r#ref,
                why: "short".to_string(),
                leg: Some(LEG_SECOND),
            },
        ),
        (
            "pool swap approved on its second leg",
            PoolStep::SwapApproved {
                r#ref,
                leg: Some(LEG_SECOND),
            },
        ),
        (
            "pool swap begun on its way back",
            PoolStep::SwapBegun {
                r#ref,
                before: Some(PoolHeld {
                    ckusdc: TokenAmount::from(0x39_u8),
                    token: TokenAmount::from(0x3a_u8),
                }),
                leg: Some(LEG_BACK),
                amount_in: Some(TokenAmount::from(0x3b_u8)),
            },
        ),
        (
            "pool swap on its way back",
            PoolStep::Swapped {
                r#ref,
                out: TokenAmount::from(0x3c_u8),
                send: Some(TokenAmount::from(0x3d_u8)),
                leg: Some(LEG_BACK),
            },
        ),
        (
            "pool swap undone on its second leg",
            PoolStep::SwapUndone {
                r#ref,
                cost: TokenAmount::from(0x3e_u8),
                withdrawn: TokenAmount::from(0x3f_u8),
                leg: Some(LEG_SECOND),
            },
        ),
    ];
    let mut out = vec![
        sample("pool ledger with its routes", ledger),
        sample(
            "pool deposit paying through ICP",
            crate::pool::Registered {
                payout: progress,
                ..pool_deposit_sample()
            },
        ),
    ];
    out.extend((0_u64..).zip(steps).map(|(i, (name, step))| {
        sample(
            name,
            Event::seal(
                EventIndex::new(4_000 + i),
                Timestamp::from_nanos(1_800_000_000_000_000_310 + i),
                EventHash::new([0xc7; 32]),
                crate::events::EventType::Pool { step },
            )
            .expect("every sample has a preimage"),
        )
    }));
    out
}

/// A swap deposit arrived and paying, for samples that change only its payout.
fn pool_deposit_sample() -> crate::pool::Registered {
    use crate::pool::{
        Arrival, CreditKey, DepositKind, DepositStatus, IcpAccount, PoolRail, Registered,
        Registration,
    };
    use candid::Principal;
    let recipient = IcpAccount {
        owner: Principal::from_slice(&[0x41; 10]),
        subaccount: None,
    };
    let mut deposit = Registered::new(Registration {
        kind: DepositKind::Swap {
            recipient,
            token_out: Principal::from_slice(&[0x33; 10]),
            min_out: 0x42,
        },
        min_usdc: TokenAmount::from(43_000_000_u32),
        premium: TokenAmount::from(44_000_u32),
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::ARBITRUM,
        src_payer: crate::evm::EvmAddress::new([0x45; 20]),
        rail: PoolRail::Eco,
        expires_at: Timestamp::from_nanos(1_800_000_000_000_000_346),
        salt: [0x47; 32],
    });
    deposit.status = DepositStatus::Paying;
    deposit.arrived = Some(Arrival {
        credit: CreditKey {
            chain: ChainId::BASE,
            tx: TxHash::new([0x48; 32]),
            log_index: 0x49,
        },
        amount: TokenAmount::from(43_000_000_u32),
        block: BlockNumber::new(37_000_350),
    });
    deposit.held = Some(TokenAmount::from(43_000_351_u32));
    deposit
}

/// The Base payouts' stored types (P11b): a payout of each kind, the ledger with its payout
/// lines, the withdrawal nonce key, and one line for each payout step.
fn payout_samples() -> Vec<Sample> {
    use crate::pool::payout::{
        EvmPayout, EvmWithdrawal, IcpSwapTerms, NonceUse, PayoutKind, PayoutLines, PayoutSettings,
        PayoutStatus, PayoutTerms, PinnedOrder, Undo,
    };
    use crate::pool::{IcpAccount, PoolAccount, PoolLedger, PoolStep, Shares};
    use candid::Principal;
    use std::collections::BTreeSet;
    use std::time::Duration;

    let principal = |byte: u8| Principal::from_slice(&[byte; 10]);
    let address = |byte: u8| crate::evm::EvmAddress::new([byte; 20]);
    let terms = PayoutTerms {
        dst_chain: ChainId::ARBITRUM,
        recipient: address(0xd1),
        token_out: address(0xd2),
        min_out: TokenAmount::from(1u128 << 80),
        deadline: Timestamp::from_nanos(1_800_000_000_000_000_211),
    };
    let swap = IcpSwapTerms {
        token_in: principal(0xd3),
        amount_in: TokenAmount::from(212_000_000_u64),
        from: IcpAccount {
            owner: principal(0xd4),
            subaccount: Some([0xd5; 32]),
        },
        min_usdc: TokenAmount::from(213_000_000_u64),
        premium: TokenAmount::from(214_000_u64),
        network_fee: TokenAmount::from(215_000_u64),
        refund_to: IcpAccount {
            owner: principal(0xd6),
            subaccount: None,
        },
    };
    let order = PinnedOrder {
        order_id: [0xd7; 32],
        order: vec![0xd8; 40],
        gas_limit: 216_000,
        deadline_s: 1_800_000_217,
    };
    let icp_payout = EvmPayout {
        id: 218,
        kind: PayoutKind::IcpSwap(swap),
        terms,
        undo: Undo::Refund { to: swap.refund_to },
        status: PayoutStatus::Signed(219),
        amount: Some(TokenAmount::from(220_000_000_u64)),
        base_fee: Some(TokenAmount::from(221_u8)),
        received: Some(TokenAmount::from(222_000_000_u64)),
        order: Some(order.clone()),
        tx_hash: Some(TxHash::new([0xdf; 32])),
        opened_at: Timestamp::from_nanos(1_800_000_000_000_000_224),
        waiting_since: Some(Timestamp::from_nanos(1_800_000_000_000_000_225)),
        queued_at: Some(226),
        paid_block: Some(BlockNumber::new(37_000_227)),
        intent: Some([0xe4; 32]),
        icp_block: Some(229),
        base_reserved: true,
        // absent, so this sample's bytes stay the ones the golden pins; the payout with its
        // legs appended after it pins them
        legs: None,
        undoing_at: None,
        icp_released: crate::pool::payout::Flag(false),
    };
    let withdrawal = EvmWithdrawal {
        staker: address(0xe6),
        usdc: TokenAmount::from(231_000_000_u64),
        max_shares: Shares::new(232 << 64),
        rebalance_share: TokenAmount::from(233_000_u64),
        network_fee: TokenAmount::from(234_000_u64),
        nonce: 235,
        burned: Shares::new(236 << 64),
    };
    let withdrawn = EvmPayout {
        id: 237,
        kind: PayoutKind::Withdrawal(withdrawal),
        undo: Undo::Remint {
            owner: PoolAccount::Evm(address(0xe6)),
            shares: Shares::new(236 << 64),
        },
        status: PayoutStatus::Undone("past its deadline".to_string()),
        received: None,
        ..icp_payout.clone()
    };
    let ledger = PoolLedger {
        payouts: Some(PayoutLines {
            settings: Some(PayoutSettings {
                min_network_fee: TokenAmount::from(238_000_u64),
                rebalance_share_floor_bps: 239,
                order_wait: Duration::from_secs(240),
                max_payout_gas: 241_000,
            }),
            next_id: 242,
            fees_icp: TokenAmount::from(243_u8),
            fees_base: TokenAmount::from(244_u8),
            queued: TokenAmount::from(245_u8),
            queue_len: 246,
            next_seq: 247,
            open: BTreeSet::from([248, 249]),
            head_since: Some(Timestamp::from_nanos(1_800_000_000_000_000_250)),
        }),
        evm_changed: None,
        ..PoolLedger::default()
    };
    let steps = [
        (
            "pool payout settings set",
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
        ),
        (
            "pool payout opened",
            PoolStep::PayoutOpened {
                id: 218,
                kind: Box::new(PayoutKind::IcpSwap(swap)),
                terms,
                undo: Undo::Refund { to: swap.refund_to },
            },
        ),
        (
            "pool payout of a withdrawal opened",
            PoolStep::PayoutOpened {
                id: 237,
                kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
                terms,
                undo: withdrawn.undo,
            },
        ),
        (
            "pool payout amount known",
            PoolStep::PayoutAmountKnown {
                id: 218,
                amount: TokenAmount::from(220_000_000_u64),
                received: Some(TokenAmount::from(222_000_000_u64)),
            },
        ),
        ("pool payout queued", PoolStep::PayoutQueued { id: 251 }),
        ("pool payout waiting", PoolStep::PayoutWaiting { id: 252 }),
        (
            "pool payout signed",
            PoolStep::PayoutSigned {
                id: 218,
                order,
                chain_id: ChainId::BASE,
                nonce: Nonce::new(253),
                tx_hash: TxHash::new([0xfe; 32]),
                raw_tx: vec![0x02, 0xfd, 0xfc],
            },
        ),
        (
            "pool payout paid",
            PoolStep::PayoutPaid {
                id: 218,
                tx_hash: TxHash::new([0xfe; 32]),
                block: BlockNumber::new(37_000_254),
                intent: Some([0xfb; 32]),
            },
        ),
        ("pool payout reverted", PoolStep::PayoutReverted { id: 255 }),
        (
            "pool payout undoing",
            PoolStep::PayoutUndoing {
                id: 256,
                why: "past its deadline".to_string(),
            },
        ),
        (
            "pool payout undone",
            PoolStep::PayoutUndone {
                id: 257,
                why: "past its deadline".to_string(),
                block: Some(258),
            },
        ),
    ];
    let mut out = vec![
        sample("pool payout of an ICP swap", icp_payout),
        sample("pool payout of a withdrawal", withdrawn),
        sample("pool ledger with its payout lines", ledger),
        sample(
            "withdrawal nonce key",
            NonceUse {
                staker: address(0xe6),
                nonce: 259,
            },
        ),
    ];
    out.extend((0_u64..).zip(steps).map(|(i, (name, step))| {
        sample(
            name,
            Event::seal(
                EventIndex::new(3_000 + i),
                Timestamp::from_nanos(1_800_000_000_000_000_260 + i),
                EventHash::new([0xc6; 32]),
                crate::events::EventType::Pool { step },
            )
            .expect("every sample has a preimage"),
        )
    }));
    out
}

/// The pool's stored types (P11a): its ledger cell, a deposit, a share line, and the three
/// map keys.
fn pool_samples() -> Vec<Sample> {
    use crate::hash::{OwnerKey, PoolOpId, PoolRef};
    use crate::pool::{
        Arrival, CreditKey, DepositKind, DepositStatus, IcpAccount, IcpCheck, IcpToken, LedgerFee,
        Paid, PayoutAttempt, PayoutPath, PayoutProgress, PendingOp, Pocket, Pockets, PoolAccount,
        PoolLedger, PoolRail, PoolSettings, Registered, Registration, ShareLine, Shares, SwapLeg,
        UnseenArrival,
    };
    use candid::Principal;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    let principal = |byte: u8| Principal::from_slice(&[byte; 10]);
    let account = IcpAccount {
        owner: principal(0x71),
        subaccount: Some([0x72; 32]),
    };
    let credit = CreditKey {
        chain: ChainId::BASE,
        tx: TxHash::new([0x73; 32]),
        log_index: 0x74,
    };
    let attempt = PayoutAttempt {
        created_at: Timestamp::from_nanos(1_800_000_000_000_000_075),
        memo: [0x76; 32],
        path: PayoutPath::Icpswap {
            pool: principal(0x77),
        },
        amount: None,
        token: None,
    };
    let ledger = PoolLedger {
        settings: Some(PoolSettings {
            tvl_cap: TokenAmount::from(10_000_000_078_u64),
            per_swap_cap: TokenAmount::from(1_000_000_079_u64),
            premium_bps: 80,
            ckusdc_ledger: principal(0x81),
            icp_tokens: vec![IcpToken {
                ledger: principal(0x82),
                icpswap_pool: principal(0x83),
            }],
            deposit_grace: Duration::from_secs(132),
            max_open_per_payer: 5,
            max_held_bps: 4_096,
            arrival_backup: Duration::from_secs(301),
            open_for_swaps: true,
            ledger_fees: vec![LedgerFee {
                ledger: principal(0x81),
                fee: TokenAmount::from(10_084_u32),
            }],
            routes: None,
        }),
        pockets: Pockets {
            base: Pocket {
                free: TokenAmount::from(85_u8),
                reserved: TokenAmount::from(86_u8),
                in_flight: TokenAmount::from(87_u8),
            },
            icp: Pocket {
                free: TokenAmount::from(1u128 << 70),
                reserved: TokenAmount::from(88_u8),
                in_flight: TokenAmount::from(89_u8),
            },
        },
        total_shares: Shares::new(1 << 100),
        owed: TokenAmount::from(90_u8),
        stake_room: TokenAmount::from(91_u8),
        open: BTreeSet::from([PoolRef::new([0x92; 32])]),
        paying: BTreeSet::from([PoolRef::new([0x93; 32])]),
        open_by_party: BTreeMap::from([(OwnerKey::new([0x94; 32]), 2)]),
        pending: BTreeMap::from([(
            PoolOpId::new([0x95; 32]),
            PendingOp::Withdraw {
                owner: account,
                burned: Shares::new(96),
                usdc: TokenAmount::from(97_u8),
                to: IcpAccount {
                    owner: principal(0x98),
                    subaccount: None,
                },
                token_out: principal(0x99),
                min_out: 0x9a,
                payout: PayoutProgress {
                    attempt: Some(attempt),
                    swap: SwapLeg {
                        declined: 1,
                        begun: true,
                        out: Some(TokenAmount::from(0x9b_u8)),
                        // absent, so this sample's bytes stay the ones the golden pins
                        send: None,
                        approvals: None,
                        undone: None,
                        before: None,
                        withdrawn: None,
                        amount_in: None,
                    },
                    failures: 3,
                    fell_back: None,
                    failed: Some("the swap's outcome is unknown".to_string()),
                    second: None,
                    back: None,
                },
            },
        )]),
        unseen: vec![UnseenArrival {
            chain: ChainId::BASE,
            block: BlockNumber::new(37_000_156),
            amount: TokenAmount::from(157_u8),
        }],
        unattributed: TokenAmount::from(158_u8),
        icp_shortfall_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_159)),
        icp_check: Some(IcpCheck {
            observed: TokenAmount::from(160_u8),
            expected: TokenAmount::from(161_u8),
            shortfall: true,
        }),
        icp_checked_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_162)),
        icp_failed_at: Some(Timestamp::from_nanos(1_800_000_000_000_000_163)),
        scan_cursor: Some(BlockNumber::new(37_000_164)),
        // empty, so this sample's bytes stay the ones the golden pins; the ledger appended
        // at the end of the pool's samples pins it
        queue: crate::pool::PayoutQueue::default(),
        // absent, as the line above pins it; the last ledger sample pins it
        head_since: None,
        // absent, so this sample's bytes stay the ones the golden pins; the ledger appended
        // after the step samples pins it
        scan_log: None,
        // absent, so this sample's bytes stay the ones the golden pins; the ledger with its
        // payout lines appended at the end pins it
        payouts: None,
        // never stored
        evm_changed: None,
    };
    let queued = PoolLedger {
        queue: crate::pool::PayoutQueue(vec![
            (PoolRef::new([0xb5; 32]), TokenAmount::from(182_u8)),
            (PoolRef::new([0xb6; 32]), TokenAmount::from(1u128 << 70)),
        ]),
        ..ledger.clone()
    };
    let waiting = PoolLedger {
        head_since: Some(Timestamp::from_nanos(1_800_000_000_000_000_183)),
        ..queued.clone()
    };
    let deposit = Registered {
        terms: Registration {
            kind: DepositKind::Swap {
                recipient: account,
                token_out: principal(0xa5),
                min_out: 0xa6,
            },
            min_usdc: TokenAmount::from(167_000_000_u32),
            premium: TokenAmount::from(168_000_u32),
            rebalance_share: TokenAmount::ZERO,
            src_chain: ChainId::ARBITRUM,
            src_payer: "0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"
                .parse()
                .unwrap(),
            rail: PoolRail::Eco,
            expires_at: Timestamp::from_nanos(1_800_000_000_000_000_169),
            salt: [0xaa; 32],
        },
        status: DepositStatus::PayoutFailed("the pool did not answer".to_string()),
        arrived: Some(Arrival {
            credit,
            amount: TokenAmount::from(171_000_000_u32),
            block: BlockNumber::new(37_000_172),
        }),
        paid: Some(Paid {
            token: principal(0xad),
            amount: TokenAmount::from(174_u8),
            block: 175,
            spent: TokenAmount::from(176_u8),
        }),
        minted: Some(Shares::new(177)),
        payout: PayoutProgress {
            attempt: Some(attempt),
            swap: SwapLeg::default(),
            failures: 10,
            fell_back: Some("below the minimum twice".to_string()),
            failed: None,
            second: None,
            back: None,
        },
        // absent, so this sample's bytes stay the ones the golden pins; the deposit
        // appended at the end of the pool's samples pins it
        held: None,
    };
    // the deposit above with what it holds of the ICP pocket once it arrived
    let holding = Registered {
        status: DepositStatus::Paying,
        paid: None,
        minted: None,
        held: Some(TokenAmount::from(171_030_000_u32)),
        ..deposit.clone()
    };
    // the deposit above with its payout's transfers fixed: the attempt's amount and ledger,
    // the output's transfer, the approvals counted, a swap undone
    let fixed = Registered {
        payout: PayoutProgress {
            attempt: Some(PayoutAttempt {
                amount: Some(TokenAmount::from(184_000_000_u32)),
                token: Some(crate::pool::PrincipalBytes(principal(0xb9))),
                ..attempt
            }),
            swap: SwapLeg {
                declined: 1,
                begun: true,
                out: Some(TokenAmount::from(185_u8)),
                send: Some(TokenAmount::from(186_u8)),
                approvals: Some(2),
                undone: Some(TokenAmount::from(187_u8)),
                // absent, so this sample's bytes stay the ones the golden pins; the deposit
                // appended after it pins them
                before: None,
                withdrawn: None,
                amount_in: None,
            },
            failures: 1,
            fell_back: None,
            failed: None,
            second: None,
            back: None,
        },
        ..holding.clone()
    };
    // the deposit above with what its pool held before its swap, and what an undo withdrew
    let mut undone = fixed.clone();
    undone.payout.swap.before = Some(crate::pool::PoolHeld {
        ckusdc: TokenAmount::from(188_u8),
        token: TokenAmount::from(189_u8),
    });
    undone.payout.swap.withdrawn = Some(TokenAmount::from(190_u8));
    // the waiting ledger above, its read of Base standing inside a block
    let reading = PoolLedger {
        scan_log: Some(205),
        ..waiting.clone()
    };
    vec![
        sample("pool ledger", ledger),
        sample("pool deposit", deposit),
        sample(
            "pool share line",
            ShareLine {
                account: PoolAccount::Evm(
                    "0x1111111111111111111111111111111111111111"
                        .parse()
                        .unwrap(),
                ),
                shares: Shares::new(178 << 64),
            },
        ),
        sample("pool ref key", PoolRef::new([0xb3; 32])),
        sample("pool owner key", OwnerKey::new([0xb4; 32])),
        sample("credited transfer key", credit),
        sample("pool deposit with what it holds", holding),
        sample("pool ledger with payouts queued", queued),
        sample("pool ledger whose queue's head waits", waiting),
        sample("pool deposit whose payout fixed its transfers", fixed),
        sample("pool deposit whose swap knew what its pool held", undone),
    ]
    .into_iter()
    .chain(pool_step_samples())
    .chain([sample("pool ledger reading inside a block", reading)])
    .collect()
}

/// One stored line for each pool step this round added or gave new fields, with every new
/// field present.
fn pool_step_samples() -> Vec<Sample> {
    use crate::hash::PoolRef;
    use crate::pool::{PayoutAttempt, PayoutPath, PoolHeld, PoolStep, PrincipalBytes};
    use candid::Principal;

    let r#ref = PoolRef::new([0xc1; 32]);
    let ckusdc = PrincipalBytes(Principal::from_slice(&[0xc2; 10]));
    let steps = [
        (
            "pool payout started with its exact transfer",
            PoolStep::PayoutStarted {
                r#ref,
                attempt: PayoutAttempt {
                    created_at: Timestamp::from_nanos(1_800_000_000_000_000_195),
                    memo: [0xc3; 32],
                    path: PayoutPath::Icpswap {
                        pool: Principal::from_slice(&[0xc4; 10]),
                    },
                    amount: Some(TokenAmount::from(196_000_000_u32)),
                    token: Some(ckusdc),
                },
            },
        ),
        (
            "pool payout fallen back with its exact transfer",
            PoolStep::FellBack {
                r#ref,
                why: "the pool refused the swap".to_string(),
                amount: Some(TokenAmount::from(197_000_000_u32)),
                token: Some(ckusdc),
            },
        ),
        (
            "pool swap begun with what its pool held",
            PoolStep::SwapBegun {
                r#ref,
                before: Some(PoolHeld {
                    ckusdc: TokenAmount::from(198_u8),
                    token: TokenAmount::from(199_u8),
                }),
                leg: None,
                amount_in: None,
            },
        ),
        (
            "pool swap with its output's transfer",
            PoolStep::Swapped {
                r#ref,
                out: TokenAmount::from(200_000_u32),
                send: Some(TokenAmount::from(198_000_u32)),
                leg: None,
            },
        ),
        (
            "pool swap approved",
            PoolStep::SwapApproved { r#ref, leg: None },
        ),
        (
            "pool swap undone",
            PoolStep::SwapUndone {
                r#ref,
                cost: TokenAmount::from(20_000_u32),
                withdrawn: TokenAmount::from(201_000_000_u32),
                leg: None,
            },
        ),
        (
            "pool read of Base done through a log of a block",
            PoolStep::ScanCursor {
                block: BlockNumber::new(37_000_203),
                log: Some(204),
            },
        ),
    ];
    (0_u64..)
        .zip(steps)
        .map(|(i, (name, step))| {
            sample(
                name,
                Event::seal(
                    EventIndex::new(2_000 + i),
                    Timestamp::from_nanos(1_800_000_000_000_000_202 + i),
                    EventHash::new([0xc5; 32]),
                    crate::events::EventType::Pool { step },
                )
                .expect("every sample has a preimage"),
            )
        })
        .collect()
}

/// The swap depths the P10c default holds, spelled out for the golden lines that pin them.
fn depths_with_ethereum_swaps_at_one_block() -> BTreeMap<ChainId, BlockDepth> {
    BTreeMap::from([
        (ChainId::ETHEREUM, BlockDepth::new(1)),
        (ChainId::BASE, BlockDepth::new(1)),
        (ChainId::BSC, BlockDepth::new(1)),
        (ChainId::POLYGON, BlockDepth::new(6)),
        (ChainId::ARBITRUM, BlockDepth::new(1)),
    ])
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
