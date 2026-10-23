//! Built on real mainnet receipts of the staging canaries' own transactions (2026-10-19,
//! `samples/`, captured read-only by the controller). A pool deposit pays the canister, and
//! these paid users, so each test rewrites only what makes them a pool deposit, and says
//! which: the recipient of the transfer that delivers (and of the Runner's `PaidOut`, of the
//! CCTP burn body, of the Eco route), and the `ref`. Emitters, topic0s, blocks, log indices
//! and every other field stay as the chains wrote them.

use super::*;
use crate::storage::events::event_count;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{account, at, ckusdc, funded_pool, payer, usdc, FEE};
use crate::storage::pool::{deposit, ledger, record_at};
use alloy_sol_types::SolEvent;
use types::pool::{DepositKind, DepositTerms, OrphanStatus};

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
        usdc: address("0xaf88d065e77c8cc2239327c5edb3a432268e5831"),
        runner: address("0x7e345acab79102ac9588c60b9358ec4d79438eee"),
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

/// A ckUSDC swap's terms for `account(byte)`: a premium of 0.01 and one ckUSDC fee of ICP
/// fees, its path's own.
fn terms(byte: u8) -> DepositTerms {
    DepositTerms {
        kind: DepositKind::Swap {
            recipient: account(byte),
            token_out: ckusdc(),
            min_out: 1,
        },
        premium: TokenAmount::from(10_000_u32),
        icp_fees: TokenAmount::from(FEE),
        rebalance_share: TokenAmount::ZERO,
        salt: [byte; 32],
    }
}

/// `terms(byte)` as a report hands them over, its source named Arbitrum.
fn given(byte: u8) -> Given {
    Given {
        terms: terms(byte),
        r#ref: terms(byte).pool_ref().unwrap(),
        src_chain: ChainId::ARBITRUM,
    }
}

/// A judge of `given` with the pool open and nobody sanctioned.
fn judge(given: Option<&Given>) -> Judge<'_> {
    Judge {
        given,
        closed: None,
        sanctioned: &|_| false,
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
    terms(byte).pool_ref().unwrap()
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

/// No registration (P11d): a report's terms credit only the transfer whose proof carries
/// their ref, at once, on what arrived; the user pays every fee, so the pool holds the
/// arrival less the premium.
#[test]
fn the_terms_credit_the_transfer_that_carries_their_ref() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = same_chain(the_ref(1));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        let given = given(1);
        let recorded = record(&found, &judge(Some(&given)), now);
        assert!(recorded.carried);
        assert_eq!(
            recorded.mine,
            vec![PoolArrivalOutcome::Credited {
                r#ref: the_ref(1).into_bytes().to_vec(),
                amount: candid::Nat::from(1_000_000_u32)
            }]
        );
        let credited = deposit(&the_ref(1)).unwrap();
        assert_eq!(credited.status, DepositStatus::Arrived);
        assert_eq!(credited.terms.min_usdc, TokenAmount::from(1_000_000_u32));
        assert_eq!(credited.terms.src_payer, payer());
        assert_eq!(credited.terms.rail, PoolRail::SameChain);
        assert_eq!(
            credited.terms.src_chain,
            ChainId::BASE,
            "proven on Base alone"
        );
        assert_eq!(credited.terms.icp_fees(), TokenAmount::from(FEE));
        assert_eq!(credited.held(), TokenAmount::from(990_000_u32));
        assert!(ledger().open.is_empty(), "nothing is left open");
    });
}

/// A transfer whose proof carries another ref than the terms' is not theirs: it waits for
/// its own terms, 24 hours, on the unattributed line, and the report is no credit.
#[test]
fn another_refs_transfer_awaits_its_own_terms() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = same_chain(the_ref(9));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        let given = given(1);
        let recorded = record(&found, &judge(Some(&given)), now);
        assert!(!recorded.carried);
        assert!(recorded.mine.is_empty());
        assert!(matches!(
            &recorded.outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("awaiting its terms")
        ));
        assert_eq!(ledger().unattributed, TokenAmount::from(1_000_000_u32));
        let orphan = read_state(|state| state.store().pool_orphan(&found[0].credit)).unwrap();
        assert_eq!(
            orphan.status,
            OrphanStatus::AwaitingTerms {
                r#ref: the_ref(9),
                until: now.checked_add(AWAIT_TERMS).unwrap()
            }
        );
        assert_eq!(orphan.payer, Some(payer()));
        assert!(deposit(&the_ref(9)).is_none());
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
        let found = find(&parse(&raw), None, &base()).unwrap();
        let given = given(2);
        let first = record(&found, &judge(Some(&given)), now).outcomes;
        assert_eq!(
            first,
            vec![PoolArrivalOutcome::Credited {
                r#ref: r#ref.into_bytes().to_vec(),
                amount: candid::Nat::from(4_747_709_u32)
            }]
        );
        let count = event_count();
        assert_eq!(record(&found, &judge(Some(&given)), now).outcomes, vec![]);
        assert_eq!(
            record(&found, &Judge::without_terms(), now).outcomes,
            vec![]
        );
        assert_eq!(event_count(), count, "nothing more was written");
        assert_eq!(deposit(&r#ref).unwrap().status, DepositStatus::Arrived);
        assert_eq!(ledger().pockets.base.free, TokenAmount::from(4_747_709_u32));
        assert!(crate::storage::pool::credited(&found[0].credit));
        // a CCTP arrival's payer is on its source chain: none bound
        assert_eq!(deposit(&r#ref).unwrap().terms.src_payer, EvmAddress::ZERO);
    });
}

/// One deposit per ref: a second transfer under a credited ref is held for a controller with
/// its payer.
#[test]
fn a_second_arrival_under_a_credited_ref_is_held() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = same_chain(the_ref(1));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let given = given(1);
        let found = find(&parse(&raw), None, &base()).unwrap();
        record(&found, &judge(Some(&given)), now);
        let mut again = same_chain(the_ref(1));
        log_at(&mut again, "0x3c8")["logIndex"] = Value::String("0x3c9".into());
        let found = find(&parse(&again), None, &base()).unwrap();
        let recorded = record(&found, &judge(Some(&given)), now);
        assert!(matches!(
            &recorded.mine[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("credited already")
        ));
        assert!(matches!(
            last_step(),
            PoolStep::Unattributed { src_payer: Some(p), r#ref: None, .. } if p == payer()
        ));
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
        eco_portal: None,
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
        let small = TokenAmount::from(1_000_000_u32);
        assert_eq!(deep_enough(block, block, ChainId::BASE, small), Ok(()));
        assert_eq!(
            deep_enough(
                block,
                BlockNumber::new(block.get() - 1),
                ChainId::BASE,
                small
            ),
            Err(PoolError::NotDeep { needed: 1, have: 0 })
        );
    });
}

/// 2026-10-23: a deposit up to $1,000 is credited after one Base block, a bigger one only
/// six blocks deep (the settings' defaults), or as deep as the settings say. Not deep yet
/// is asked again (the scan's cursor stops before it), never refused for good.
#[test]
fn a_large_deposit_waits_its_own_depth() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let block = BlockNumber::new(52_823_000);
        let head = |deep: u64| BlockNumber::new(block.get() + deep - 1);
        let deep = |depth: u64, largest: u64| {
            deep_enough(block, head(depth), ChainId::BASE, usdc(largest))
        };
        assert_eq!(deep(1, 1_000), Ok(()));
        assert_eq!(
            deep(1, 1_001),
            Err(PoolError::NotDeep { needed: 6, have: 1 })
        );
        assert_eq!(
            deep(5, 1_001),
            Err(PoolError::NotDeep { needed: 6, have: 5 })
        );
        assert_eq!(deep(6, 1_001), Ok(()));
        let mut settings = ledger().settings.unwrap();
        settings.large_deposit_usdc = Some(usdc(100));
        settings.large_deposit_confirmations = Some(3);
        record_at(PoolStep::SettingsSet { settings }, at(2)).unwrap();
        assert_eq!(deep(1, 100), Ok(()));
        assert_eq!(deep(2, 101), Err(PoolError::NotDeep { needed: 3, have: 2 }));
        assert_eq!(deep(3, 101), Ok(()));
    });
}

/// Terms the pool does not take hold the transfer for a controller, with the terms and the
/// payer the proof shows: ICP fees below the path's own, the pool closed, a sanctioned
/// party. Nothing is registered. A swap above the settings' per-swap cap is no such term
/// (2026-10-23: no cap on an EVM to ICP deposit): it is credited.
#[test]
fn terms_the_pool_does_not_take_hold_the_transfer() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = cctp(the_ref(2));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        // a per-swap cap below the 4.747709 that arrived
        let mut settings = ledger().settings.unwrap();
        settings.per_swap_cap = usdc(4);
        record_at(PoolStep::SettingsSet { settings }, now).unwrap();
        let swap = given(2);
        let held = |found: &[Found], judge: &Judge<'_>| -> String {
            match &record(found, judge, now).mine[..] {
                [PoolArrivalOutcome::Unattributed { why, .. }] => why.clone(),
                other => panic!("not held: {other:?}"),
            }
        };
        assert!(matches!(
            &record(&found, &judge(Some(&swap)), now).mine[..],
            [PoolArrivalOutcome::Credited { .. }]
        ));
        assert_eq!(deposit(&the_ref(2)).unwrap().status, DepositStatus::Arrived);
        // the ICP fees below the path's own ckUSDC fee
        let mut cheap = swap.clone();
        cheap.terms.icp_fees = TokenAmount::from(FEE - 1);
        cheap.r#ref = cheap.terms.pool_ref().unwrap();
        let raw = same_chain(cheap.r#ref);
        let found = find(&parse(&raw), None, &base()).unwrap();
        let why = held(&found, &judge(Some(&cheap)));
        assert!(why.contains("the ICP fees must be at least"), "{why}");
        // the pool closed, a sanctioned payer
        let mut other = same_chain(the_ref(1));
        log_at(&mut other, "0x3c8")["logIndex"] = Value::String("0x3ca".into());
        let found = find(&parse(&other), None, &base()).unwrap();
        let mine = given(1);
        let closed = Judge {
            closed: Some("the books are off".to_string()),
            ..judge(Some(&mine))
        };
        assert!(held(&found, &closed).contains("the pool is closed: the books are off"));
        let mut other = same_chain(the_ref(1));
        log_at(&mut other, "0x3c8")["logIndex"] = Value::String("0x3cb".into());
        let found = find(&parse(&other), None, &base()).unwrap();
        let sanctioned = Judge {
            sanctioned: &|address| *address == Address::from(payer()),
            ..judge(Some(&mine))
        };
        assert!(held(&found, &sanctioned).contains("the payer is sanctioned"));
        assert!(deposit(&the_ref(1)).is_none());
    });
}

/// The canister's own read finds a deposit's transfer without its terms: it waits for them.
/// Anyone hands them over within the 24 hours: claimed off the unattributed line and
/// credited once. Past the 24 hours it is a controller's.
#[test]
fn a_transfer_awaiting_its_terms_is_claimed_by_them() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = cctp(the_ref(2));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        record(&found, &Judge::without_terms(), now);
        assert_eq!(ledger().unattributed, TokenAmount::from(4_747_709_u32));
        let base_free = ledger().pockets.base.free;
        // past its 24 hours: a controller's, and the terms credit nothing
        let given = given(2);
        let late = now
            .checked_add(AWAIT_TERMS)
            .unwrap()
            .checked_add(Duration::from_secs(1))
            .unwrap();
        let recorded = record(&found, &judge(Some(&given)), late);
        assert!(recorded.mine.is_empty());
        assert!(recorded.not[0].contains("past its 24 hours"));
        // within them: claimed and credited
        let later = now.checked_add(Duration::from_secs(3_600)).unwrap();
        let recorded = record(&found, &judge(Some(&given)), later);
        assert_eq!(
            recorded.mine,
            vec![PoolArrivalOutcome::Credited {
                r#ref: the_ref(2).into_bytes().to_vec(),
                amount: candid::Nat::from(4_747_709_u32)
            }]
        );
        assert_eq!(ledger().unattributed, TokenAmount::ZERO);
        assert_eq!(
            ledger().pockets.base.free,
            base_free
                .checked_add(TokenAmount::from(4_747_709_u32))
                .unwrap()
        );
        assert_eq!(deposit(&the_ref(2)).unwrap().status, DepositStatus::Arrived);
        let orphan = read_state(|state| state.store().pool_orphan(&found[0].credit)).unwrap();
        assert_eq!(orphan.status, OrphanStatus::Credited);
        // once
        let count = event_count();
        assert!(record(&found, &judge(Some(&given)), later).mine.is_empty());
        assert_eq!(event_count(), count);
    });
}

/// Terms that came for a transfer awaiting them but that the pool does not take hold it for
/// a controller, with why, still on the unattributed line.
#[test]
fn terms_refused_for_an_awaiting_transfer_hold_it() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let raw = cctp(the_ref(2));
        let now = Timestamp::from_secs(block_time(&raw) + 5).unwrap();
        let found = find(&parse(&raw), None, &base()).unwrap();
        record(&found, &Judge::without_terms(), now);
        let given = given(2);
        let closed = Judge {
            closed: Some("the books are off".to_string()),
            ..judge(Some(&given))
        };
        assert!(matches!(
            &record(&found, &closed, now).mine[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("the pool is closed")
        ));
        let orphan = read_state(|state| state.store().pool_orphan(&found[0].credit)).unwrap();
        assert_eq!(orphan.status, OrphanStatus::Held);
        assert_eq!(ledger().unattributed, TokenAmount::from(4_747_709_u32));
        assert!(deposit(&the_ref(2)).is_none());
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

/// P13a's convention: for a CCTP or a same-chain arrival the watcher sends the Base
/// transaction as its own source (`src_chain = 8453`, `src_tx = base_tx`). The proof is in
/// the Base receipt alone: no source is read for them, and one given changes nothing.
#[test]
fn a_cctp_or_same_chain_arrival_ignores_its_source() {
    let base_as_source = SourceChain {
        chain: ChainId::BASE,
        swapic: base().swapic,
        eco_portal: base().eco_portal,
        usdc: base().usdc,
        runner: base().runner,
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
        let now = Timestamp::from_secs(time + 5).unwrap();
        let fill = parse(&fill);
        let count = event_count();
        let wrong = parse(&sample("eco_source"));
        let given = given(3);
        let found = find(&fill, Some((&wrong, &arbitrum())), &base()).unwrap();
        let recorded = record(&found, &judge(Some(&given)), now);
        assert!(recorded.outcomes.is_empty());
        assert_eq!(recorded.not.len(), 1);
        assert_eq!(
            event_count(),
            count,
            "nothing written, nothing marked credited"
        );
        assert!(!crate::storage::pool::credited(&found[0].credit));
        // its own report credits it, its source chain the one the report read
        let found = find(&fill, Some((&parse(&source), &arbitrum())), &base()).unwrap();
        assert!(matches!(
            &record(&found, &judge(Some(&given)), now).mine[..],
            [PoolArrivalOutcome::Credited { .. }]
        ));
        let credited = deposit(&r#ref).unwrap();
        assert_eq!(credited.terms.rail, PoolRail::Eco);
        assert_eq!(credited.terms.src_chain, ChainId::ARBITRUM);
        assert_eq!(credited.terms.src_payer, payer());
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
            rail: PoolRail::Eco,
            proof: Proof::Orphan {
                payer: None,
                why: "test".into(),
            },
        };
        let recorded = record(&[refused, found[0].clone()], &Judge::without_terms(), now);
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
/// the canister the payout's amount. A payout delivering into that intent is unfilled by it
/// (undone next, never paid again) and its USDC credited back to the Base pocket, once; a
/// second refund and an unknown intent's are orphans.
#[test]
fn an_eco_refund_credits_a_delivering_payout_once() {
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
        let recorded = record(&found, &Judge::without_terms(), at(5));
        assert!(recorded.not.is_empty() || !recorded.outcomes.is_empty());
        // the refund proves the intent unfilled: the payout is undone next, never paid
        // again, and its USDC is back in the Base pocket's free line, once
        let refunded = crate::storage::pool::payout(id).unwrap();
        assert_eq!(refunded.status, PayoutStatus::Unfilled);
        assert_eq!(refunded.reclaim, Some(types::pool::Reclaim::Credited));
        let after = ledger();
        let fee = paid.base_fee.unwrap();
        assert_eq!(
            after.pockets.base.free,
            before
                .pockets
                .base
                .free
                .checked_add(amount)
                .and_then(|sum| sum.checked_add(fee))
                .unwrap()
        );
        assert_eq!(after.pockets.base.in_flight, TokenAmount::ZERO);
        assert_eq!(after.owed, before.owed);
        // credited once
        assert!(record(&found, &Judge::without_terms(), at(6))
            .outcomes
            .is_empty());
        // a second refund of the same amount for the same intent: nothing owed, kept for a
        // human
        let mut again = found.clone();
        again[0].credit.tx = TxHash::new([0x3f; 32]);
        assert!(matches!(
            record(&again, &Judge::without_terms(), at(6))
                .outcomes
                .as_slice(),
            [PoolArrivalOutcome::Unattributed { .. }]
        ));
        // another intent's refund: no paid payout takes it, kept for a human
        let stranger = parse(&refund([0x2e; 32]));
        let mut found = find(&stranger, None, &base()).unwrap();
        found[0].credit.tx = TxHash::new([0x2f; 32]);
        let recorded = record(&found, &Judge::without_terms(), at(7));
        assert!(matches!(
            recorded.outcomes.as_slice(),
            [PoolArrivalOutcome::Unattributed { .. }]
        ));
    });
}

/// A stranger, Mallory, who pays from her own address.
fn stranger() -> EvmAddress {
    address("0x00000000000000000000000000000000000bad01")
}

/// The Eco source `source` (the user's, from `eco`) as the stranger's own transaction would
/// show it: her own order under her own ref, and the user's intent beside it.
fn strangers_order(mut source: Value, r#ref: PoolRef) -> Value {
    let started = log_at(&mut source, "0xe");
    set_topic(started, 2, r#ref.into_bytes());
    set_topic(started, 3, stranger().to_word());
    source
}

/// What the stranger's transaction on another chain shows, at its least (H-02): her own
/// zero-amount order under her own ref, and the user's `IntentPublished` copied byte for byte
/// (the intent's hash commits no source chain), never funded.
fn copied_intent(source: &Value, r#ref: PoolRef) -> Value {
    let mut copy = strangers_order(source.clone(), r#ref);
    let keep: Vec<Value> = ["0xe", "0x11"]
        .iter()
        .map(|index| log_at(&mut copy, index).clone())
        .collect();
    copy["logs"] = Value::Array(keep);
    copy
}

/// H-02: a stranger who copies the user's Eco intent beside her own order and reports the
/// user's Base fill first is credited nothing, and leaves the transfer for the user's own
/// report, which credits it on the user's terms.
#[test]
fn a_copied_intent_beside_a_strangers_order_is_no_deposit() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let (users, strangers) = (given(3), given(4));
        let (fill, source) = eco(users.r#ref);
        let now = Timestamp::from_secs(block_time(&fill) + 5).unwrap();
        let fill = parse(&fill);
        let count = event_count();
        let copy = parse(&copied_intent(&source, strangers.r#ref));
        let found = find(&fill, Some((&copy, &arbitrum())), &base()).unwrap();
        assert!(
            matches!(found[0].proof, Proof::Unproven { .. }),
            "{:?}",
            found[0].proof
        );
        let recorded = record(&found, &judge(Some(&strangers)), now);
        assert!(recorded.outcomes.is_empty());
        assert_eq!(event_count(), count, "nothing written for the stranger");
        assert!(!crate::storage::pool::credited(&found[0].credit));
        assert!(deposit(&strangers.r#ref).is_none());
        // the user's own report credits the transfer on the user's terms
        let found = find(&fill, Some((&parse(&source), &arbitrum())), &base()).unwrap();
        assert!(matches!(
            &record(&found, &judge(Some(&users)), now).mine[..],
            [PoolArrivalOutcome::Credited { .. }]
        ));
        assert_eq!(deposit(&users.r#ref).unwrap().terms.src_payer, payer());
    });
}

/// H-02, each binding on its own: the matched intent's creator is the order's payer (the
/// whole funded shape of the user's transaction with only the order the stranger's), and its
/// funding, by the source Runner and in full, is in the same transaction.
#[test]
fn an_eco_deposit_binds_its_intent_to_its_order() {
    let (fill, source) = eco(the_ref(3));
    let fill = parse(&fill);
    let unproven = |source: &Value| {
        let found = find(&fill, Some((&parse(source), &arbitrum())), &base()).unwrap();
        matches!(found[0].proof, Proof::Unproven { .. })
    };
    assert!(!unproven(&source), "the real shape proves its deposit");
    assert!(
        unproven(&strangers_order(source.clone(), the_ref(4))),
        "the intent's creator is not the order's payer"
    );
    let mut unfunded = source.clone();
    unfunded["logs"]
        .as_array_mut()
        .unwrap()
        .retain(|log| log["logIndex"] != "0x13");
    assert!(unproven(&unfunded), "nothing funded the intent");
    let mut by_another = source.clone();
    let funded = log_at(&mut by_another, "0x13");
    let mut data = data_of(funded);
    data[32..64].copy_from_slice(&stranger().to_word());
    set_data(funded, &data);
    assert!(
        unproven(&by_another),
        "someone other than the Runner funded it"
    );
    let mut in_part = source.clone();
    let funded = log_at(&mut in_part, "0x13");
    let mut data = data_of(funded);
    data[64..96].copy_from_slice(&[0; 32]);
    set_data(funded, &data);
    assert!(unproven(&in_part), "the intent was funded in part only");
    // the user's whole transaction, named as published on a chain whose USDC is another
    // token: a copy there rewards in a token that is not that chain's USDC
    let elsewhere = SourceChain {
        chain: ChainId::new(10),
        usdc: address("0x0b2c639c533813f4aa9d7837caf62653d097ff85"),
        ..arbitrum()
    };
    let found = find(&fill, Some((&parse(&source), &elsewhere)), &base()).unwrap();
    assert!(
        matches!(found[0].proof, Proof::Unproven { .. }),
        "the reward is not the source chain's USDC"
    );
}

/// H-02: an intent the canister created, beside its own order, is a rebalance move's fill and
/// never a deposit, even under terms whose ref its order carries; with no open move to take
/// it, it is held for a controller.
#[test]
fn an_intent_the_canister_created_is_a_moves_never_a_deposit() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        let given = given(3);
        let (fill, mut source) = eco(given.r#ref);
        let now = Timestamp::from_secs(block_time(&fill) + 5).unwrap();
        set_topic(log_at(&mut source, "0xe"), 3, canister().to_word());
        set_topic(log_at(&mut source, "0x11"), 2, canister().to_word());
        let fill = parse(&fill);
        let found = find(&fill, Some((&parse(&source), &arbitrum())), &base()).unwrap();
        assert_eq!(found[0].proof, Proof::Move { r#ref: given.r#ref });
        let recorded = record(&found, &judge(Some(&given)), now);
        assert!(!recorded.carried);
        assert!(matches!(
            &recorded.outcomes[..],
            [PoolArrivalOutcome::Unattributed { why, .. }] if why.contains("canister created")
        ));
        assert!(deposit(&given.r#ref).is_none());
    });
}

/// M-03: the receipt every node agreed on is taken only beside a header of the very block
/// one node named as the head: a head no node sees as a block is no depth, and asked again;
/// no receipt yet is asked again; another transaction's receipt never reads.
#[test]
fn an_agreed_receipt_stands_beside_the_head_every_node_sees() {
    let raw = same_chain(the_ref(1));
    let tx = parse(&raw).tx;
    let head = BlockNumber::new(0x100);
    let header = |number: &str| {
        Ok(serde_json::json!({"number": number, "hash": "0xab", "timestamp": "0x6ad5e897"}))
    };
    assert_eq!(
        agreed_receipt(tx, head, vec![Ok(raw.clone()), header("0x100")]),
        Ok(parse(&raw))
    );
    for (receipt, header) in [
        (Ok(raw.clone()), Ok(Value::Null)),
        (Ok(raw.clone()), header("0x101")),
        (Ok(Value::Null), header("0x100")),
        (
            Err(RpcError::Unreachable {
                reason: crate::rpc::Summary::of("no consensus"),
            }),
            header("0x100"),
        ),
    ] {
        assert!(matches!(
            agreed_receipt(tx, head, vec![receipt, header]),
            Err(PoolError::Unavailable(_))
        ));
    }
    assert!(matches!(
        agreed_receipt(TxHash::new([9; 32]), head, vec![Ok(raw), header("0x100")]),
        Err(PoolError::BadArrival(_))
    ));
}

/// Review of the merged canister (item 3): a refund whose payout's pinned order names no route
/// deadline that parses is read past the same time the queue reads that payout's fill (its
/// order's deadline and the longest route the canister signs), never held for a human for
/// it: the payout's `PayoutUnfilled` is always matched by its refund, and its reclaim never
/// stays pending.
#[test]
fn a_refund_whose_route_does_not_parse_is_read_as_the_queue_reads_it() {
    use crate::storage::pool::tests::{sign_payout, waiting_withdrawal};
    struct Portal;
    impl FillReads for Portal {
        async fn fill(
            &self,
            _: ChainId,
            _: [u8; 32],
            route_deadline: u64,
        ) -> Result<types::eco::Fill, String> {
            // `sign_payout` pins an order of deadline 900 whose bytes are no order
            assert_eq!(route_deadline, 900 + crate::pool::eco::MAX_ROUTE_LIFE_S);
            Ok(types::eco::Fill::Open)
        }
    }
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
        );
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
        let found = vec![Found {
            credit: CreditKey {
                chain: ChainId::BASE,
                tx: TxHash::new([9; 32]),
                log_index: 3,
            },
            amount: TokenAmount::from(1_u32),
            block: BlockNumber::new(40),
            at: None,
            rail: PoolRail::Eco,
            proof: Proof::Refund {
                intents: vec![intent],
            },
        }];
        use std::future::Future;
        let late = Timestamp::from_secs(900 + crate::pool::eco::MAX_ROUTE_LIFE_S + 3_600).unwrap();
        let mut future = std::pin::pin!(prove_refunds(found, &Portal, late));
        let context = &mut std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(proven) = future.as_mut().poll(context) else {
            panic!("nothing waits");
        };
        assert_eq!(
            proven[0].proof,
            Proof::Refund {
                intents: vec![intent]
            }
        );
        let payout = crate::storage::pool::payout(id).unwrap();
        assert_eq!(
            crate::pool::queue::fill_read_after(&payout),
            900 + crate::pool::eco::MAX_ROUTE_LIFE_S,
            "the queue reads it past the same time"
        );
    });
}

/// The last review (item 1): the depth is counted from the block every node agreed exists,
/// one below the one node's head, never from that head itself. A node that claims a head two
/// blocks past the real one names a block no node sees, and is refused; one block past, it
/// names the real head, and the receipt is judged on the real chain's depth: a head a node
/// lies about gains no depth.
#[test]
fn a_head_a_node_lies_about_gains_no_depth() {
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        // the sample's 1 USDC is a large deposit here: it waits 6 blocks
        let mut settings = ledger().settings.unwrap();
        settings.large_deposit_usdc = Some(TokenAmount::from(500_000_u32));
        record_at(PoolStep::SettingsSet { settings }, at(2)).unwrap();
        let raw = same_chain(the_ref(1));
        let receipt = parse(&raw);
        // the real chain: four blocks from the receipt's own
        let real = BlockNumber::new(receipt.block.get() + 3);
        // what every node answers for block `n`: a header up to the real head, none past it
        let nodes_see = |n: BlockNumber| {
            Ok(if n <= real {
                serde_json::json!({"number": format!("0x{:x}", n.get()), "hash": "0xab", "timestamp": "0x6ad5e897"})
            } else {
                Value::Null
            })
        };
        let judged = |claimed: u64| {
            let claimed = BlockNumber::new(claimed);
            let named = named_block(claimed);
            based(
                receipt.tx,
                claimed,
                vec![Ok(raw.clone()), nodes_see(named)],
                &base(),
            )
        };
        // two past the real head: the block it names is no block every node sees
        assert!(matches!(
            judged(real.get() + 2),
            Err(PoolError::Unavailable(_))
        ));
        // one past: judged on the real head, 4 of the 6 blocks
        assert_eq!(
            judged(real.get() + 1),
            Err(PoolError::NotDeep { needed: 6, have: 4 })
        );
        // the honest head: one block below it, 3 of the 6
        assert_eq!(
            judged(real.get()),
            Err(PoolError::NotDeep { needed: 6, have: 3 })
        );
        // two blocks more of the real chain, and an honest node: deep
        let deeper = BlockNumber::new(real.get() + 3);
        let named = named_block(deeper);
        assert_eq!(
            based(
                receipt.tx,
                deeper,
                vec![
                    Ok(raw.clone()),
                    Ok(
                        serde_json::json!({"number": format!("0x{:x}", named.get()), "hash": "0xab", "timestamp": "0x6ad5e897"})
                    )
                ],
                &base(),
            ),
            Ok(receipt)
        );
    });
}
