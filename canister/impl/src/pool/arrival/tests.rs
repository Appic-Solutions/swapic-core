//! Built on real mainnet receipts of the staging canaries' own transactions (2026-10-19,
//! `samples/`, captured read-only by the controller). A pool deposit pays the canister, and
//! these paid users, so each test rewrites only what makes them a pool deposit, and says
//! which: the recipient of the transfer that delivers (and of the Runner's `PaidOut`, of the
//! CCTP burn body, of the Eco route), and the `ref`. Emitters, topic0s, blocks, log indices
//! and every other field stay as the chains wrote them.

use super::*;
use crate::storage::events::event_count;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, ckusdc, funded_pool, payer, usdc};
use crate::storage::pool::{deposit, ledger, record_at};
use alloy_sol_types::SolEvent;
use types::pool::{DepositKind, PoolRail, Registration};

/// The staging canister's address on every chain.
fn canister() -> EvmAddress {
    "0x36D34038F04990a58c068e08eD9754e2E3D9bA53"
        .parse()
        .unwrap()
}

fn address(text: &str) -> EvmAddress {
    parse_address(text).unwrap()
}

fn base() -> PoolChain {
    PoolChain {
        chain: ChainId::BASE,
        usdc: address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
        runner: address("0x7e345acab79102ac9588c60b9358ec4d79438eee"),
        swapic: address("0xf0ba68cbc87c31ecaa9654eb14daf3632eaca406"),
        transmitter: address("0x81d40f21f12a8f0e3252bccb954d722d4c464b64"),
        eco_portal: Some(address("0xEC000769A73b70e16f361a442292500b3BCf4A85")),
        canister: canister(),
        // the CCTP sample's burn left Ethereum (domain 0) through its TokenMessengerV2, made
        // by Swapic's Runner (one address on every chain)
        cctp_sources: vec![CctpSource {
            domain: 0,
            token_messenger: address("0x28b5a0e9c621a5badaa536219b3a228c8168cf5d"),
            runner: address("0x7e345acab79102ac9588c60b9358ec4d79438eee"),
        }],
    }
}

fn arbitrum() -> SourceChain {
    SourceChain {
        chain: ChainId::ARBITRUM,
        swapic: address("0xf0ba68cbc87c31ecaa9654eb14daf3632eaca406"),
        eco_portal: Some(address("0xEC000769A73b70e16f361a442292500b3BCf4A85")),
    }
}

fn sample(name: &str) -> Value {
    let text = match name {
        "cctp" => include_str!("samples/cctp_deliver_base.json"),
        "same_chain" => include_str!("samples/same_chain_base.json"),
        "eco_source" => include_str!("samples/eco_source_arbitrum.json"),
        "eco_fill" => include_str!("samples/eco_fill_base.json"),
        _ => unreachable!(),
    };
    serde_json::from_str::<Value>(text).unwrap()["receipt"].clone()
}

fn log_at<'a>(receipt: &'a mut Value, index: &str) -> &'a mut Value {
    receipt["logs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|log| log["logIndex"] == index)
        .unwrap()
}

fn set_topic(log: &mut Value, at: usize, word: [u8; 32]) {
    log["topics"][at] = Value::String(hex0x(&word));
}

fn data_of(log: &Value) -> Vec<u8> {
    hex::decode(log["data"].as_str().unwrap().strip_prefix("0x").unwrap()).unwrap()
}

fn set_data(log: &mut Value, data: &[u8]) {
    log["data"] = Value::String(hex0x(data));
}

fn word(amount: u64) -> [u8; 32] {
    TokenAmount::from(amount).to_be_bytes()
}

/// The block time of every sample's logs is about this, in seconds.
fn block_time(receipt: &Value) -> u64 {
    hex_u64(receipt["logs"][0].get("blockTimestamp")).unwrap()
}

fn terms(byte: u8, min_usdc: TokenAmount, expires_at: Timestamp, rail: PoolRail) -> Registration {
    Registration {
        kind: DepositKind::Swap {
            recipient: account(byte),
            token_out: ckusdc(),
            min_out: 1,
        },
        min_usdc,
        premium: TokenAmount::from(10_000_u32),
        rebalance_share: TokenAmount::ZERO,
        src_chain: ChainId::BASE,
        src_payer: payer(),
        rail,
        expires_at,
        salt: [byte; 32],
    }
}

/// The same-chain sample (USDC to ether on Base, user-paid) as a pool deposit: the Runner's
/// USDC transfer at 0x3c8 goes to the canister instead of KyberSwap's executor, `PaidOut` at
/// 0x3d2 pays the canister that USDC (its recipient topic and its token and amount words),
/// and `OrderStarted` at 0x3c6 carries `r#ref`.
fn same_chain(r#ref: PoolRef) -> Value {
    let mut receipt = sample("same_chain");
    set_topic(log_at(&mut receipt, "0x3c8"), 2, canister().to_word());
    let paid = log_at(&mut receipt, "0x3d2");
    set_topic(paid, 2, canister().to_word());
    let mut data = base().usdc.to_word().to_vec();
    data.extend_from_slice(&word(1_000_000));
    set_data(paid, &data);
    set_topic(log_at(&mut receipt, "0x3c6"), 2, r#ref.into_bytes());
    receipt
}

/// The CCTP sample (the canister's `receiveMessage` of a Deliver into Base) as a pool
/// deposit: the mint at 0x4a goes to the canister, and in `MessageReceived` at 0x4e the
/// burn body's mint recipient is the canister and its envelope's ref is `r#ref`.
fn cctp(r#ref: PoolRef) -> Value {
    let mut receipt = sample("cctp");
    set_topic(log_at(&mut receipt, "0x4a"), 2, canister().to_word());
    let received = log_at(&mut receipt, "0x4e");
    let mut data = data_of(received);
    // the body starts after three head words and its length word
    data[128 + 36..128 + 68].copy_from_slice(&canister().to_word());
    data[128 + 228 + 32..128 + 228 + 64].copy_from_slice(&r#ref.into_bytes());
    set_data(received, &data);
    receipt
}

/// The Eco samples (a gasless order on Arbitrum filled on Base) as a pool deposit: on Base
/// the route's final transfer at 0x5f goes to the canister, not the Runner; on Arbitrum
/// `OrderStarted` at 0xe carries `r#ref`, and the route `IntentPublished` at 0x11 carries is
/// the Deliver shape a pool deposit's is, `USDC.transfer(canister, value)` (the sample's
/// was a swap fill), every other field of the event as published.
fn eco(r#ref: PoolRef) -> (Value, Value) {
    let mut fill = sample("eco_fill");
    let transfer = log_at(&mut fill, "0x5f");
    set_topic(transfer, 2, canister().to_word());
    let value = U256Word::of(&data_of(transfer));
    let mut source = sample("eco_source");
    set_topic(log_at(&mut source, "0xe"), 2, r#ref.into_bytes());
    let published = log_at(&mut source, "0x11");
    let topics: Vec<alloy_primitives::FixedBytes<32>> = published["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|topic| alloy_primitives::FixedBytes(parse_hash32(Some(topic)).unwrap()))
        .collect();
    let mut event =
        types::swapic::IntentPublished::decode_raw_log(topics, &data_of(published)).unwrap();
    let route = types::swapic::route_delivery(&event.route);
    assert_eq!(route, None, "the sample's route is a fill, not a delivery");
    event.route = types::swapic::deliver_route(
        [0x5a; 32],
        1_800_000_000,
        base().eco_portal.unwrap(),
        base().usdc,
        canister(),
        value.0,
    )
    .into();
    set_data(published, &event.encode_data());
    (fill, source)
}

struct U256Word(alloy_primitives::U256);

impl U256Word {
    fn of(data: &[u8]) -> Self {
        Self(alloy_primitives::U256::from_be_slice(data))
    }
}

fn parse(value: &Value) -> Receipt {
    Receipt::parse(value).unwrap()
}

fn the_ref(byte: u8) -> PoolRef {
    terms(byte, usdc(1), at(0), PoolRail::Cctp)
        .pool_ref()
        .unwrap()
}

#[test]
fn a_same_chain_deposit_is_proven() {
    let r#ref = the_ref(1);
    let receipt = parse(&same_chain(r#ref));
    let found = find(&receipt, None, &base()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].amount, TokenAmount::from(1_000_000_u32));
    assert_eq!(found[0].credit.log_index, 0x3c8);
    assert_eq!(found[0].credit.tx, receipt.tx);
    assert_eq!(
        found[0].proof,
        Proof::Deposit {
            r#ref,
            payer: Some(payer())
        }
    );
    assert!(found[0].at.is_some(), "the provider gives the block's time");
}

#[test]
fn a_cctp_deposit_is_proven() {
    let r#ref = the_ref(2);
    let receipt = parse(&cctp(r#ref));
    let found = find(&receipt, None, &base()).unwrap();
    assert_eq!(found.len(), 1);
    // the burn less Circle's fee: 4748183 - 474
    assert_eq!(found[0].amount, TokenAmount::from(4_747_709_u32));
    assert_eq!(found[0].credit.log_index, 0x4a);
    assert_eq!(found[0].proof, Proof::Deposit { r#ref, payer: None });
}

#[test]
fn an_eco_deposit_is_proven_on_its_source() {
    let r#ref = the_ref(3);
    let (fill, source) = eco(r#ref);
    let (fill, source) = (parse(&fill), parse(&source));
    assert!(needs_source(&fill, &base()));
    assert_eq!(find(&fill, None, &base()), Err(NoArrival::NeedsSource));
    let found = find(&fill, Some((&source, &arbitrum())), &base()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].amount, TokenAmount::from(4_705_087_u32));
    assert_eq!(found[0].credit.log_index, 0x5f);
    assert_eq!(
        found[0].proof,
        Proof::Deposit {
            r#ref,
            payer: Some(payer())
        }
    );
    // a source whose intent does not deliver this transfer to the canister proves nothing,
    // and leaves the transfer for the report that brings its own source
    let untouched = parse(&sample("eco_source"));
    let found = find(&fill, Some((&untouched, &arbitrum())), &base()).unwrap();
    assert!(matches!(found[0].proof, Proof::Unproven { .. }));
}

/// A transfer to someone else, or of another token, is no arrival at all: nothing is
/// recorded.
#[test]
fn a_transfer_to_someone_else_or_of_another_token_is_no_arrival() {
    for name in ["cctp", "same_chain", "eco_fill"] {
        let receipt = parse(&sample(name));
        assert_eq!(
            find(&receipt, None, &base()),
            Err(NoArrival::NoTransfer),
            "{name}"
        );
    }
    let weth = PoolChain {
        usdc: address("0x4200000000000000000000000000000000000006"),
        ..base()
    };
    let receipt = parse(&same_chain(the_ref(1)));
    assert_eq!(find(&receipt, None, &weth), Err(NoArrival::NoTransfer));
    // a reverted transaction proves nothing
    let mut reverted = same_chain(the_ref(1));
    reverted["status"] = Value::String("0x0".into());
    assert_eq!(
        find(&parse(&reverted), None, &base()),
        Err(NoArrival::Reverted)
    );
}

/// A wrong ref: proven, and registered nowhere, so unattributed with its payer.
#[test]
fn a_wrong_ref_is_unattributed_with_its_payer() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = same_chain(the_ref(9));
        let receipt = parse(&raw);
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        crate::storage::pool::tests::register(terms(
            1,
            usdc(1),
            Timestamp::from_secs(block_time(&raw) + 600).unwrap(),
            PoolRail::SameChain,
        ));
        let found = find(&receipt, None, &base()).unwrap();
        let outcomes = record(&found, now).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why == "no deposit is registered under the ref"
        ));
        assert_eq!(ledger().unattributed, TokenAmount::from(1_000_000_u32));
        assert!(matches!(
            deposit(&the_ref(1)).unwrap().status,
            DepositStatus::Registered
        ));
    });
}

/// Review Focus 1: the same transfer reported twice (two calls, or a call and the backup
/// timer) is credited once.
#[test]
fn the_same_transfer_reported_twice_is_credited_once() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(2);
        let raw = cctp(r#ref);
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        crate::storage::pool::tests::register(terms(
            2,
            usdc(4),
            Timestamp::from_secs(block_time(&raw) + 600).unwrap(),
            PoolRail::Cctp,
        ));
        let found = find(&parse(&raw), None, &base()).unwrap();
        let first = record(&found, now).outcomes;
        assert_eq!(
            first,
            vec![PoolArrivalOutcome::Credited {
                r#ref: r#ref.into_bytes().to_vec(),
                amount: candid::Nat::from(4_747_709_u32)
            }]
        );
        let count = event_count();
        assert_eq!(record(&found, now).outcomes, vec![]);
        assert_eq!(event_count(), count, "nothing more was written");
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Arrived);
        assert_eq!(ledger().pockets.base.free, TokenAmount::from(4_747_709_u32));
        assert!(crate::storage::pool::credited(&found[0].credit));
    });
}

/// Review Focus 4: an arrival past its registration's expiry and grace (at its block's
/// time) is unattributed with the registered payer, never credited, and its hold goes back.
#[test]
fn a_late_arrival_is_unattributed_with_its_payer() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(3);
        let (fill, source) = eco(r#ref);
        let time = block_time(&fill);
        let grace = types::pool::PoolSettings::default().deposit_grace.as_secs();
        // expired, and the grace ran out one second before the fill's block
        crate::storage::pool::tests::register(terms(
            3,
            usdc(4),
            Timestamp::from_secs(time - grace - 1).unwrap(),
            PoolRail::Eco,
        ));
        let found = find(&parse(&fill), Some((&parse(&source), &arbitrum())), &base()).unwrap();
        // read late by the canister: judged at the block's time, not the read's
        let outcomes = record(&found, Timestamp::from_secs(time + 3_600).unwrap()).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why == "arrived past its expiry and grace"
        ));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Unattributed);
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
        let orphan = crate::storage::events::event_at(event_count() - 1).unwrap();
        assert!(matches!(
            orphan.payload,
            types::EventType::Pool {
                step: PoolStep::Unattributed { src_payer: Some(p), .. }
            } if p == payer()
        ));
    });
}

/// An arrival on time at its block, read after its grace by the backup, is credited.
#[test]
fn an_arrival_on_time_read_late_is_credited() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(3);
        let (fill, source) = eco(r#ref);
        let time = block_time(&fill);
        crate::storage::pool::tests::register(terms(
            3,
            usdc(4),
            Timestamp::from_secs(time).unwrap(),
            PoolRail::Eco,
        ));
        let found = find(&parse(&fill), Some((&parse(&source), &arbitrum())), &base()).unwrap();
        let outcomes = record(&found, Timestamp::from_secs(time + 3_600).unwrap()).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Credited { .. }]
        ));
    });
}

/// Below its registered minimum, or paid by another than the registered payer: kept
/// unattributed; another payer's money leaves the user's registration open.
#[test]
fn a_short_arrival_or_another_payers_is_unattributed() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = same_chain(the_ref(1));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let expires = Timestamp::from_secs(block_time(&raw) + 600).unwrap();
        crate::storage::pool::tests::register(terms(1, usdc(2), expires, PoolRail::SameChain));
        let found = find(&parse(&raw), None, &base()).unwrap();
        let outcomes = record(&found, now).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("below the")
        ));
        assert_eq!(
            deposit(&the_ref(1)).unwrap().status,
            DepositStatus::Unattributed
        );

        // another payer
        let mut other = terms(2, usdc(1), expires, PoolRail::SameChain);
        other.src_payer = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let r#ref = other.pool_ref().unwrap();
        crate::storage::pool::tests::register(other);
        let mut raw = same_chain(r#ref);
        // another log index, so it is another transfer
        log_at(&mut raw, "0x3c8")["logIndex"] = Value::String("0x3c9".into());
        let found: Vec<Found> = find(&parse(&raw), None, &base()).unwrap();
        let outcomes = record(&found, now).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why == "paid by another than the registered payer"
        ));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
    });
}

/// A same-chain gasless order pays the canister its fee step and, as a pool deposit, its
/// payout, both as Runner transfers: the fee step is no arrival, and the books book only
/// the fee step. The fee step here is the Arbitrum sample's real one (log 0xf, the Runner to
/// the canister), its emitter rewritten to Base's USDC.
#[test]
fn the_fee_step_is_the_books_and_the_payout_is_the_pools() {
    let r#ref = the_ref(1);
    let mut receipt = same_chain(r#ref);
    let mut fee = log_at(&mut sample("eco_source"), "0xf").clone();
    fee["address"] = Value::String(base().usdc.to_string());
    fee["logIndex"] = Value::String("0x3d3".into());
    let fee_value = TokenAmount::from_be_bytes(data_of(&fee).try_into().unwrap());
    receipt["logs"].as_array_mut().unwrap().push(fee);
    let found = find(&parse(&receipt), None, &base()).unwrap();
    assert_eq!(found.len(), 1, "the fee step is no arrival");
    assert_eq!(found[0].amount, TokenAmount::from(1_000_000_u32));
    let path = crate::books::FeePath {
        usdc: base().usdc,
        runner: base().runner,
        canister: canister(),
    };
    assert_eq!(crate::books::network_fee_in(&receipt, path), fee_value);
    // a receipt of the fee step alone is the books', and no arrival
    let mut only_fee = sample("eco_source");
    log_at(&mut only_fee, "0xf")["address"] = Value::String(base().usdc.to_string());
    let only_fee = parse(&only_fee);
    assert_eq!(find(&only_fee, None, &base()), Err(NoArrival::OnlyFees));
}

/// The crediting depth: 1 block on Base, read off the config.
#[test]
fn a_receipt_not_deep_enough_is_not_credited() {
    on_fresh_memory(|| {
        crate::storage::init();
        let block = BlockNumber::new(52_823_000);
        assert_eq!(deep_enough(block, block, ChainId::BASE), Ok(()));
        assert_eq!(
            deep_enough(block, BlockNumber::new(block.get() - 1), ChainId::BASE),
            Err(PoolError::NotDeep { needed: 1, have: 0 })
        );
    });
}

/// Review fix: an arrival larger than its registration is measured on what arrived. Above
/// the per-swap cap it is no credit. A CCTP arrival's payer is not bound, so it is kept with
/// no ref and no payer, and the registration keeps its hold.
#[test]
fn an_arrival_above_the_cap_is_unattributed() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(2);
        let raw = cctp(r#ref);
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        crate::storage::pool::tests::register(terms(
            2,
            usdc(4),
            Timestamp::from_secs(block_time(&raw) + 600).unwrap(),
            PoolRail::Cctp,
        ));
        // a per-swap cap below the 4.747709 that arrived
        let mut settings = ledger().settings.unwrap();
        settings.per_swap_cap = usdc(4);
        record_at(PoolStep::SettingsSet { settings }, now).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        let outcomes = record(&found, now).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("above the per-swap cap")
        ));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
        assert_eq!(ledger().pockets.icp.reserved, TokenAmount::ZERO);
        assert!(ledger().open.contains(&r#ref));
        // kept with no ref, the registration's payer its refund target
        assert!(matches!(
            last_step(),
            PoolStep::Unattributed {
                src_payer: Some(p),
                r#ref: None,
                ref why,
                ..
            } if p == payer() && why.ends_with("(payer presumed: the registration's)")
        ));
    });
}

fn last_step() -> PoolStep {
    match crate::storage::events::event_at(event_count() - 1)
        .unwrap()
        .payload
    {
        types::EventType::Pool { step } => step,
        other => panic!("not a pool line: {other:?}"),
    }
}

/// Review fix (CCTP binding): a mint to the canister proves a deposit only when Swapic's
/// Runner burned it on a chain the canister knows, through that chain's TokenMessengerV2.
/// Anyone else's burn carrying someone's ref proves nothing.
#[test]
fn a_cctp_burn_is_swapics_runners_or_proves_nothing() {
    let r#ref = the_ref(2);
    let orphaned = |receipt: &Value, chain: &PoolChain| {
        matches!(
            find(&parse(receipt), None, chain).unwrap()[..],
            [Found {
                proof: Proof::Orphan { .. },
                ..
            }]
        )
    };
    // the real burn, as Swapic's Runner made it
    assert!(!orphaned(&cctp(r#ref), &base()));
    // a burn by another sender: the burn body's messageSender rewritten
    let mut stranger = cctp(r#ref);
    let received = log_at(&mut stranger, "0x4e");
    let mut data = data_of(received);
    data[128 + 100..128 + 132].copy_from_slice(&EvmAddress::new([0x99; 20]).to_word());
    set_data(received, &data);
    assert!(orphaned(&stranger, &base()));
    // a source chain the canister does not know, or another messenger on it
    let unknown = PoolChain {
        cctp_sources: vec![],
        ..base()
    };
    assert!(orphaned(&cctp(r#ref), &unknown));
    let other_messenger = PoolChain {
        cctp_sources: vec![CctpSource {
            token_messenger: EvmAddress::new([0x98; 20]),
            ..base().cctp_sources[0]
        }],
        ..base()
    };
    assert!(orphaned(&cctp(r#ref), &other_messenger));
}

/// Review fix (CCTP binding, and the loss of user funds): a CCTP arrival's payer is not
/// bound, so one short of its registration is kept with no ref: it never closes the
/// registration nor releases its hold (a stranger's burn under someone's ref). It is kept
/// with the registration's payer as its refund target, so a user's own short arrival is
/// never lost, and a stranger's only goes back to that payer.
#[test]
fn a_short_cctp_arrival_never_closes_a_registration() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(2);
        let raw = cctp(r#ref);
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        crate::storage::pool::tests::register(terms(
            2,
            usdc(10),
            Timestamp::from_secs(block_time(&raw) + 600).unwrap(),
            PoolRail::Cctp,
        ));
        let held = ledger().pockets.icp.reserved;
        let found = find(&parse(&raw), None, &base()).unwrap();
        let outcomes = record(&found, now).outcomes;
        assert!(matches!(
            &outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("below the")
        ));
        // kept with no ref, the registration's payer its refund target
        assert!(matches!(
            last_step(),
            PoolStep::Unattributed {
                src_payer: Some(p),
                r#ref: None,
                ref why,
                ..
            } if p == payer() && why.ends_with("(payer presumed: the registration's)")
        ));
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Registered);
        assert_eq!(ledger().pockets.icp.reserved, held);
        assert!(ledger().open.contains(&r#ref));
    });
}

/// P13a's convention: for a CCTP or a same-chain arrival the watcher sends the Base
/// transaction as its own source (`src_chain = 8453`, `src_tx = base_tx`). The proof is in
/// the Base receipt alone: no source is read for them, and one given changes nothing.
#[test]
fn a_cctp_or_same_chain_arrival_ignores_its_source() {
    let base_as_source = SourceChain {
        chain: ChainId::BASE,
        swapic: base().swapic,
        eco_portal: base().eco_portal,
    };
    for raw in [cctp(the_ref(2)), same_chain(the_ref(1))] {
        let receipt = parse(&raw);
        assert!(!needs_source(&receipt, &base()));
        assert_eq!(
            find(&receipt, Some((&receipt, &base_as_source)), &base()),
            find(&receipt, None, &base())
        );
    }
}

/// Review fix: an Eco transfer the given source does not prove is left uncredited, with no
/// payer borrowed from that source: its own report, with its own source, credits it.
#[test]
fn an_eco_transfer_its_source_does_not_prove_waits_for_its_own_report() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let r#ref = the_ref(3);
        let (fill, source) = eco(r#ref);
        let time = block_time(&fill);
        crate::storage::pool::tests::register(terms(
            3,
            usdc(4),
            Timestamp::from_secs(time + 600).unwrap(),
            PoolRail::Eco,
        ));
        let now = Timestamp::from_secs(time + 5).unwrap();
        let fill = parse(&fill);
        let count = event_count();
        let wrong = parse(&sample("eco_source"));
        let found = find(&fill, Some((&wrong, &arbitrum())), &base()).unwrap();
        let recorded = record(&found, now);
        assert!(recorded.outcomes.is_empty());
        assert_eq!(recorded.not.len(), 1);
        assert_eq!(
            event_count(),
            count,
            "nothing written, nothing marked credited"
        );
        assert!(!crate::storage::pool::credited(&found[0].credit));
        // its own report credits it
        let found = find(&fill, Some((&parse(&source), &arbitrum())), &base()).unwrap();
        assert!(matches!(
            &record(&found, now).outcomes[..],
            [PoolArrivalOutcome::Credited { .. }]
        ));
    });
}

/// Review fix: two mints of one value in one transaction are each matched to their own
/// received message, so each is proven under its own ref. The second mint and message are
/// copies of the CCTP sample's (logs 0x4a and 0x4e) at two new log indices, the second
/// message's envelope carrying another ref.
#[test]
fn two_equal_mints_are_each_their_own_messages() {
    let (first, second) = (the_ref(2), the_ref(4));
    let mut raw = cctp(first);
    let mut mint = log_at(&mut raw, "0x4a").clone();
    let mut message = log_at(&mut raw, "0x4e").clone();
    mint["logIndex"] = Value::String("0x50".into());
    message["logIndex"] = Value::String("0x51".into());
    let mut data = data_of(&message);
    data[128 + 228 + 32..128 + 228 + 64].copy_from_slice(&second.into_bytes());
    set_data(&mut message, &data);
    let logs = raw["logs"].as_array_mut().unwrap();
    logs.push(mint);
    logs.push(message);
    let found = find(&parse(&raw), None, &base()).unwrap();
    let proven: Vec<_> = found
        .iter()
        .map(|one| (one.credit.log_index, one.proof.clone()))
        .collect();
    assert_eq!(
        proven,
        vec![
            (
                0x4a,
                Proof::Deposit {
                    r#ref: first,
                    payer: None
                }
            ),
            (
                0x50,
                Proof::Deposit {
                    r#ref: second,
                    payer: None
                }
            ),
        ]
    );
}

/// Review fix: a transfer of nothing is no arrival, and one transfer's refused step never
/// stops the others of the receipt from being recorded.
#[test]
fn a_zero_transfer_is_nothing_and_one_refusal_stops_no_other() {
    let mut raw = same_chain(the_ref(1));
    let mut zero = log_at(&mut raw, "0x3c8").clone();
    zero["logIndex"] = Value::String("0x3f0".into());
    set_topic(&mut zero, 1, EvmAddress::new([0x77; 20]).to_word());
    set_data(&mut zero, &word(0));
    raw["logs"].as_array_mut().unwrap().push(zero);
    let found = find(&parse(&raw), None, &base()).unwrap();
    assert_eq!(found.len(), 1, "the zero transfer is not read");

    on_fresh_memory(move || {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        // a step the fold refuses (an orphan of nothing), then a good one
        let refused = Found {
            credit: CreditKey {
                chain: ChainId::BASE,
                tx: TxHash::new([0x0c; 32]),
                log_index: 1,
            },
            amount: TokenAmount::ZERO,
            block: BlockNumber::new(1),
            at: None,
            proof: Proof::Orphan {
                payer: None,
                why: "test".into(),
            },
        };
        let recorded = record(&[refused, found[0].clone()], now);
        assert_eq!(recorded.not.len(), 1);
        assert_eq!(recorded.outcomes.len(), 1);
    });
}

/// Review (backup, denial of service): a receipt too large for the read is refused for good,
/// never as a passing trouble to ask again; any other failed read is one.
#[test]
fn a_receipt_too_large_is_refused_for_good() {
    assert!(matches!(
        unavailable(RpcError::AnswerTooLarge { cap: 512 }),
        PoolError::BadArrival(_)
    ));
    assert!(matches!(
        unavailable(RpcError::Unanswered {
            method: "eth_getTransactionReceipt".to_string()
        }),
        PoolError::Unavailable(_)
    ));
}

/// P11b Task 6, on the real Eco fill on Base rewritten to a refund: the Portal's log that
/// fulfilled the intent rewritten to `IntentRefunded(intentHash, refundee = the canister)`,
/// its `IntentFulfilled` dropped (no one filled it), and the USDC transfer rewritten to pay
/// the canister the payout's amount. A paid payout whose order published that intent takes
/// it back and waits again for a new order; an unknown intent's refund is an orphan.
#[test]
fn an_eco_refund_of_a_paid_payout_reopens_it() {
    use crate::storage::pool::tests::{sign_payout, waiting_withdrawal};
    use types::pool::PayoutStatus;
    on_fresh_memory(|| {
        crate::storage::init();
        let staker: EvmAddress = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let id = waiting_withdrawal(staker);
        sign_payout(id, 0, 7);
        let intent = [0x1e; 32];
        record_at(
            PoolStep::PayoutPaid {
                id,
                tx_hash: TxHash::new([7; 32]),
                block: BlockNumber::new(30),
                intent: Some(intent),
            },
            at(4),
        )
        .unwrap();
        let paid = crate::storage::pool::payout(id).unwrap();
        let amount = paid.amount.unwrap();
        let before = ledger();
        let refund = |intent: [u8; 32]| {
            let mut receipt = sample("eco_fill");
            let fulfilled = topics::intent_fulfilled();
            let portal = base().eco_portal.unwrap();
            let logs = receipt["logs"].as_array_mut().unwrap();
            let at_portal: Vec<usize> = logs
                .iter()
                .enumerate()
                .filter(|(_, log)| parse_address(log["address"].as_str().unwrap()) == Some(portal))
                .map(|(at, _)| at)
                .collect();
            let fulfilled_at = *at_portal
                .iter()
                .find(|at| parse_hash32(Some(&logs[**at]["topics"][0])) == Some(fulfilled))
                .unwrap();
            let refunded_at = *at_portal.iter().find(|at| **at != fulfilled_at).unwrap();
            logs[refunded_at]["topics"] = serde_json::json!([
                hex0x(&topics::intent_refunded()),
                hex0x(&canister().to_word()),
            ]);
            set_data(&mut logs[refunded_at], &intent);
            logs.remove(fulfilled_at);
            let transfer = log_at(&mut receipt, "0x5f");
            set_topic(transfer, 2, canister().to_word());
            set_data(transfer, &amount.to_be_bytes());
            receipt
        };
        let receipt = parse(&refund(intent));
        let found = find(&receipt, None, &base()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].proof,
            Proof::Refund {
                intents: vec![intent]
            }
        );
        let recorded = record(&found, at(5));
        assert!(recorded.not.is_empty() || !recorded.outcomes.is_empty());
        let reopened = crate::storage::pool::payout(id).unwrap();
        assert_eq!(reopened.status, PayoutStatus::Waiting);
        assert_eq!(reopened.order, None, "a new order is allowed only now");
        let after = ledger();
        let fee = paid.base_fee.unwrap();
        assert_eq!(
            after.pockets.base.reserved,
            before
                .pockets
                .base
                .reserved
                .checked_add(amount)
                .and_then(|sum| sum.checked_add(fee))
                .unwrap()
        );
        assert_eq!(after.lines().fees_base, TokenAmount::ZERO);
        assert_eq!(after.owed, before.owed.checked_add(paid.owed()).unwrap());
        // credited once
        assert!(record(&found, at(6)).outcomes.is_empty());
        // another intent's refund: no paid payout takes it, kept for a human
        let stranger = parse(&refund([0x2e; 32]));
        let mut found = find(&stranger, None, &base()).unwrap();
        found[0].credit.tx = TxHash::new([0x2f; 32]);
        let recorded = record(&found, at(7));
        assert!(matches!(
            recorded.outcomes.as_slice(),
            [PoolArrivalOutcome::Unattributed { .. }]
        ));
    });
}
