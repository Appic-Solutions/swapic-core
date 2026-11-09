//! Held to real receipts: T0's Base source (`types/src/swapic/across/samples/t0_receipts.json`),
//! a relayer's fill on Base of a BNB Chain deposit and that deposit (`samples/bsc_to_base_fill.json`,
//! a LI.FI order: its recipient stands in for the canister here), and BNB Chain's relayer refund
//! leaves of an expired deposit (`samples/bsc_refund_leaves.json`: its depositor stands in for
//! the canister, its ETH for the pool chain's USDC). No receipt of a batched fill to one
//! recipient, a slow fill, a deferred leaf or a claim was at hand: those cases are the real
//! ones above with one log changed or added, each marked ADAPTED where it is made.

use super::*;
use crate::pool::arrival::Receipt;
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use serde_json::{json, Value};

fn sample(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn fills() -> Value {
    sample(include_str!("samples/bsc_to_base_fill.json"))
}

fn leaves() -> Value {
    sample(include_str!("samples/bsc_refund_leaves.json"))
}

fn t0() -> Value {
    sample(include_str!(
        "../../../../types/src/swapic/across/samples/t0_receipts.json"
    ))
}

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn receipt(value: &Value) -> Receipt {
    Receipt::parse(value).expect("a receipt")
}

/// Base's USDC and SpokePool pin, and the fill's recipient as the canister.
fn base_pool() -> AcrossPool {
    AcrossPool {
        chain: ChainId::BASE,
        spoke_pool: address("0x09aea4b2242abc8bb4bb78d537a67a245a7bec64"),
        usdc: address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
        canister: address("0xf3b0042a0a3b6acb78122a6f6ead0f667a18059e"),
        origins: vec![ChainId::BSC],
    }
}

const RELAYER: &str = "0xfd0aa7548823c40be6dd85682f0342f35dee121a";

/// The canister's one transfer in `base`: its log index, sender and value.
fn the_transfer(base: &Receipt, pool: &AcrossPool) -> (u64, EvmAddress, TokenAmount) {
    let found = transfers(&base.logs, pool.usdc, pool.canister);
    let [(index, from, value)] = found.as_slice() else {
        panic!("one transfer to the canister");
    };
    (*index, *from, TokenAmount::from_u256(*value))
}

fn explained(base: &Receipt, pool: &AcrossPool) -> Explained {
    let (index, from, value) = the_transfer(base, pool);
    explain(&base.logs, index, from, value, pool, &mut Vec::new())
}

fn log_value(log: &Value) -> crate::pool::arrival::Log {
    crate::pool::arrival::logs_of(&json!({ "logs": [log] }))
        .pop()
        .unwrap()
}

fn encoded(address: EvmAddress, data: alloy_primitives::LogData, index: u64) -> Value {
    json!({
        "address": address.to_string(),
        "topics": data.topics().iter().map(|topic| format!("0x{}", hex::encode(topic))).collect::<Vec<_>>(),
        "data": format!("0x{}", hex::encode(&data.data)),
        "logIndex": format!("0x{index:x}"),
    })
}

#[test]
fn a_relayers_fill_to_the_canister_is_paired_by_its_identity() {
    let base = receipt(&fills()["fill"]);
    let pool = base_pool();
    let Explained::Fill(filled) = explained(&base, &pool) else {
        panic!("the fill explains the transfer");
    };
    assert_eq!(filled.relayer, word(address(RELAYER)));
    assert_eq!(filled.key.origin_chain_id, 56);
    assert_eq!(filled.fill_type, FillType::FastFill);
}

#[test]
fn a_fill_that_is_not_the_transfers_is_held() {
    let base = receipt(&fills()["fill"]);
    // from a chain the canister takes no Across deposit from
    let elsewhere = AcrossPool {
        origins: vec![ChainId::ARBITRUM],
        ..base_pool()
    };
    assert!(matches!(
        explained(&base, &elsewhere),
        Explained::Mismatch(_)
    ));
    // the pool chain's own deposit is never a deposit into the pool
    let own = AcrossPool {
        chain: ChainId::BSC,
        ..base_pool()
    };
    assert!(matches!(explained(&base, &own), Explained::Mismatch(_)));
    // another amount than the fill paid: ADAPTED, the transfer's value one unit less
    let mut raw = fills()["fill"].clone();
    let logs = raw["logs"].as_array_mut().unwrap();
    let transfer = logs
        .iter_mut()
        .find(|log| log["topics"][0] == json!(format!("0x{}", hex::encode(topics::transfer()))))
        .unwrap();
    let value = U256::from_str_radix(
        transfer["data"].as_str().unwrap().trim_start_matches("0x"),
        16,
    )
    .unwrap();
    transfer["data"] = json!(format!(
        "0x{}",
        hex::encode((value - U256::from(1u8)).to_be_bytes::<32>())
    ));
    let base = receipt(&raw);
    assert_eq!(
        explained(&base, &base_pool()),
        Explained::Mismatch("the fill pays another amount")
    );
}

#[test]
fn a_transfer_another_sent_after_the_fill_is_not_the_fills() {
    // ADAPTED: the transfer's sender is someone else than the fill's relayer
    let mut raw = fills()["fill"].clone();
    for log in raw["logs"].as_array_mut().unwrap() {
        if log["topics"][0] == json!(format!("0x{}", hex::encode(topics::transfer()))) {
            log["topics"][1] = json!(format!("0x{}{}", "0".repeat(24), "77".repeat(20)));
        }
    }
    assert_eq!(
        explained(&receipt(&raw), &base_pool()),
        Explained::Mismatch("the fill's relayer did not send it")
    );
}

#[test]
fn a_batched_fill_pairs_each_transfer_with_its_own_fill() {
    // ADAPTED: the real fill and its transfer, then a second fill to another recipient and its
    // transfer, then the real pair again under new indexes: each transfer to the canister
    // pairs with the fill right before it, never a fill an earlier canister transfer took
    let raw = fills()["fill"].clone();
    let real: Vec<Value> = raw["logs"].as_array().unwrap().clone();
    let (fill, transfer) = (real[0].clone(), real[1].clone());
    let mut other_fill = fill.clone();
    let mut data = hex::decode(
        other_fill["data"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    // the recipient word and the updated recipient word of another recipient
    for at in [32 * 9, 32 * 11] {
        data[at..at + 32].copy_from_slice(&[0u8; 32]);
        data[at + 12..at + 32].copy_from_slice(&[0x66; 20]);
    }
    other_fill["data"] = json!(format!("0x{}", hex::encode(&data)));
    let mut other_transfer = transfer.clone();
    other_transfer["topics"][2] = json!(format!("0x{}{}", "0".repeat(24), "66".repeat(20)));
    let mut logs = Vec::new();
    for (index, mut log) in [
        fill.clone(),
        transfer.clone(),
        other_fill,
        other_transfer,
        fill,
        transfer,
    ]
    .into_iter()
    .enumerate()
    {
        log["logIndex"] = json!(format!("0x{index:x}"));
        logs.push(log);
    }
    let batched = receipt(&json!({
        "transactionHash": raw["transactionHash"],
        "blockNumber": raw["blockNumber"],
        "status": "0x1",
        "logs": logs,
    }));
    let pool = base_pool();
    let found = transfers(&batched.logs, pool.usdc, pool.canister);
    assert_eq!(found.len(), 2);
    for (index, from, value) in found {
        assert!(matches!(
            explain(
                &batched.logs,
                index,
                from,
                TokenAmount::from_u256(value),
                &pool,
                &mut Vec::new()
            ),
            Explained::Fill(_)
        ));
    }
    // the second canister transfer with no fill of its own before it: the first fill is
    // behind another canister transfer, so nothing pairs it
    let mut lone = logs.clone();
    lone.remove(4);
    let lone = receipt(&json!({
        "transactionHash": raw["transactionHash"],
        "blockNumber": raw["blockNumber"],
        "status": "0x1",
        "logs": lone,
    }));
    let second = transfers(&lone.logs, pool.usdc, pool.canister)[1];
    let explained = explain(
        &lone.logs,
        second.0,
        second.1,
        TokenAmount::from_u256(second.2),
        &pool,
        &mut Vec::new(),
    );
    // the closest fill before it is the other recipient's, with no canister transfer between
    assert_eq!(
        explained,
        Explained::Mismatch("the fill is not to the canister")
    );
}

#[test]
fn a_slow_fill_comes_from_the_spoke_pool() {
    // ADAPTED: the real fill as a slow fill (fill type 2), its transfer from the SpokePool
    let mut raw = fills()["fill"].clone();
    let pool = base_pool();
    let mut data = hex::decode(
        raw["logs"][0]["data"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    let last = data.len() - 1;
    data[last] = 2;
    raw["logs"][0]["data"] = json!(format!("0x{}", hex::encode(&data)));
    raw["logs"][1]["topics"][1] = json!(format!("0x{}", hex::encode(word(pool.spoke_pool))));
    assert!(
        matches!(explained(&receipt(&raw), &pool), Explained::Fill(f) if f.fill_type == FillType::SlowFill)
    );
    // a fast fill's tokens never come from the SpokePool
    data[last] = 0;
    raw["logs"][0]["data"] = json!(format!("0x{}", hex::encode(&data)));
    assert_eq!(
        explained(&receipt(&raw), &pool),
        Explained::Mismatch("the fill's relayer did not send it")
    );
}

/// BNB Chain's leaves, its SpokePool and ETH, the expired deposit's depositor as the canister.
fn leaf_pool() -> AcrossPool {
    AcrossPool {
        chain: ChainId::BSC,
        spoke_pool: address("0x4e8e101924ede233c13e2d8622dc8aed2872d505"),
        usdc: address("0x2170ed0880ac9a755fd29b2688956bd959f933f8"),
        canister: address("0x144c5fb302dbaa789fc59bbec301169eaa56c5fc"),
        origins: vec![],
    }
}

#[test]
fn a_relayer_refund_leaf_naming_the_canister_is_an_across_refund() {
    let base = receipt(&leaves()["receipt"]);
    let pool = leaf_pool();
    assert_eq!(explained(&base, &pool), Explained::Refund);
    // each entry pays one transfer
    let (index, from, value) = the_transfer(&base, &pool);
    let mut used = Vec::new();
    assert_eq!(
        explain(&base.logs, index, from, value, &pool, &mut used),
        Explained::Refund
    );
    assert_eq!(
        explain(&base.logs, index, from, value, &pool, &mut used),
        Explained::Nothing
    );
    // another token's leaf, or another chain's, pays nothing of it
    let other = AcrossPool {
        chain: ChainId::BASE,
        ..leaf_pool()
    };
    assert_eq!(explained(&base, &other), Explained::Nothing);
    let wrong_amount = value.checked_add(TokenAmount::from(1_u8)).unwrap();
    assert_eq!(
        explain(
            &base.logs,
            index,
            from,
            wrong_amount,
            &pool,
            &mut Vec::new()
        ),
        Explained::Nothing
    );
}

#[test]
fn a_claim_counts_only_when_the_canister_made_it() {
    let pool = base_pool();
    let amount = U256::from(2_500_000u32);
    let claim = |caller: EvmAddress| {
        // ADAPTED: no claim receipt was at hand; the event as the deployed SpokePool's ABI
        // writes it
        types::swapic::across::ClaimedRelayerRefund {
            l2TokenAddress: word(pool.usdc).into(),
            refundAddress: word(pool.canister).into(),
            amount,
            caller: Address::from(*caller.as_bytes()),
        }
        .encode_log_data()
    };
    let transfer = json!({
        "address": pool.usdc.to_string(),
        "topics": [
            format!("0x{}", hex::encode(topics::transfer())),
            format!("0x{}", hex::encode(word(pool.spoke_pool))),
            format!("0x{}", hex::encode(word(pool.canister))),
        ],
        "data": format!("0x{}", hex::encode(amount.to_be_bytes::<32>())),
        "logIndex": "0x0",
    });
    for (caller, expected) in [
        (pool.canister, Explained::Refund),
        (address(RELAYER), Explained::Nothing),
    ] {
        let logs = vec![
            log_value(&transfer),
            log_value(&encoded(pool.spoke_pool, claim(caller), 1)),
        ];
        assert_eq!(
            explain(
                &logs,
                0,
                pool.spoke_pool,
                TokenAmount::from_u256(amount),
                &pool,
                &mut Vec::new()
            ),
            expected
        );
    }
}

/// The origin proof's source: the real BNB Chain deposit with the Swapic events it lacks
/// added (ADAPTED): the LI.FI contract that deposited stands in for the periphery, the
/// contract that funded it for the Runner, and the order and the periphery's swap log are the
/// ones Swapic's order would write beside them.
fn source_with(
    swap_token_amount: Option<U256>,
    runner_pays: Option<U256>,
) -> (Receipt, AcrossSource) {
    let raw = fills()["deposit"].clone();
    let real = receipt(&raw);
    let periphery = address("0x1231deb6f5749ef6ce6943a275a1d3e7486f4eae");
    let runner = address("0x20f6ee51340adeed01a59b0e65cb3703f3dc860c");
    let swapic = address("0xb3f66517bec4cc5a8e799db2dda4c810befcc917");
    let spoke_pool = address("0x4e8e101924ede233c13e2d8622dc8aed2872d505");
    let peg_usdc = address("0x8ac76a51cc950d9822d68b83fe1ad97b32cd580d");
    let payer = address("0xf3b0042a0a3b6acb78122a6f6ead0f667a18059e");
    let input = U256::from(3_001_108_910_703_358_693u64);
    let mut logs: Vec<Value> = raw["logs"]
        .as_array()
        .unwrap()
        .iter()
        // the Runner's own transfer is written below when asked
        .filter(|log| {
            !(log["topics"][0] == json!(format!("0x{}", hex::encode(topics::transfer())))
                && log["topics"][1] == json!(format!("0x{}", hex::encode(word(runner)))))
        })
        .cloned()
        .collect();
    let started = types::swapic::OrderStarted {
        id: [0x51; 32].into(),
        r#ref: [0x52; 32].into(),
        payer: Address::from(*payer.as_bytes()),
        tokenIn: Address::from(*peg_usdc.as_bytes()),
        amountIn: input,
        memo: vec![0x53].into(),
    }
    .encode_log_data();
    logs.push(encoded(swapic, started, 900));
    let swapped = types::swapic::across::SwapBeforeBridge {
        exchange: Address::from(*peg_usdc.as_bytes()),
        exchangeCalldata: vec![0x70, 0xa0, 0x82, 0x31].into(),
        swapToken: Address::from(*peg_usdc.as_bytes()),
        acrossInputToken: Address::from(*peg_usdc.as_bytes()),
        swapTokenAmount: swap_token_amount.unwrap_or(input),
        acrossInputAmount: input,
        acrossOutputToken: word(address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")).into(),
        acrossOutputAmount: U256::from(2_996_850u32),
    }
    .encode_log_data();
    logs.push(encoded(periphery, swapped, 901));
    if let Some(paid) = runner_pays {
        logs.push(json!({
            "address": peg_usdc.to_string(),
            "topics": [
                format!("0x{}", hex::encode(topics::transfer())),
                format!("0x{}", hex::encode(word(runner))),
                format!("0x{}", hex::encode(word(periphery))),
            ],
            "data": format!("0x{}", hex::encode(paid.to_be_bytes::<32>())),
            "logIndex": "0x386",
        }));
    }
    let source = receipt(&json!({
        "transactionHash": raw["transactionHash"],
        "blockNumber": raw["blockNumber"],
        "status": raw["status"],
        "logs": logs,
    }));
    assert_eq!(source.tx, real.tx);
    (
        source,
        AcrossSource {
            chain: ChainId::BSC,
            swapic,
            runner,
            pins: AcrossPins {
                periphery,
                swap_proxy: address("0x1d1735bcf925d10b600279ce720da2673c2274c5"),
                spoke_pool,
                multicall_handler: address("0x0f7ae28de1c8532170ad4ee566b5801485c13a0e"),
            },
        },
    )
}

fn the_fill() -> Filled {
    let base = receipt(&fills()["fill"]);
    match explained(&base, &base_pool()) {
        Explained::Fill(filled) => *filled,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_fill_is_proven_on_its_deposit_the_order_and_the_runners_funding() {
    let input = U256::from(3_001_108_910_703_358_693u64);
    let (source, at) = source_with(None, Some(input));
    assert_eq!(
        origin_proof(&the_fill(), &source, &at, &base_pool()),
        Proof::Deposit {
            r#ref: PoolRef::new([0x52; 32]),
            payer: Some(address("0xf3b0042a0a3b6acb78122a6f6ead0f667a18059e")),
        }
    );
}

#[test]
fn a_gift_left_in_the_swap_proxy_is_tolerated() {
    // the Runner put in less than the deposit holds: the rest was a stranger's gift (M2)
    let input = U256::from(3_001_108_910_703_358_693u64);
    let pulled = input - U256::from(1u8);
    let (source, at) = source_with(Some(pulled), Some(pulled));
    assert!(matches!(
        origin_proof(&the_fill(), &source, &at, &base_pool()),
        Proof::Deposit { .. }
    ));
    // more pulled than deposited is no identity deposit
    let (source, at) = source_with(Some(input + U256::from(1u8)), Some(input + U256::from(1u8)));
    assert!(matches!(
        origin_proof(&the_fill(), &source, &at, &base_pool()),
        Proof::Unproven { .. }
    ));
}

#[test]
fn a_deposit_the_runner_did_not_fund_or_not_the_fills_is_unproven() {
    let input = U256::from(3_001_108_910_703_358_693u64);
    // no transfer from the Runner
    let (source, at) = source_with(None, None);
    assert!(matches!(
        origin_proof(&the_fill(), &source, &at, &base_pool()),
        Proof::Unproven { why } if why.contains("Runner")
    ));
    // a fill of another deposit id
    let (source, at) = source_with(None, Some(input));
    let mut other = the_fill();
    other.key.deposit_id += U256::from(1u8);
    assert!(matches!(
        origin_proof(&other, &source, &at, &base_pool()),
        Proof::Unproven { .. }
    ));
    // a fill that repeats another output
    let mut other = the_fill();
    other.key.output_amount += U256::from(1u8);
    assert!(matches!(
        origin_proof(&other, &source, &at, &base_pool()),
        Proof::Unproven { .. }
    ));
    // read on another chain than the deposit's
    let elsewhere = AcrossSource {
        chain: ChainId::ARBITRUM,
        ..at
    };
    assert!(matches!(
        origin_proof(&the_fill(), &source, &elsewhere, &base_pool()),
        Proof::Unproven { .. }
    ));
}

#[test]
fn the_canisters_own_deposit_is_read_off_its_paid_receipt() {
    // T0's Base source: its payer stands in for the canister
    let source = t0()["source"].clone();
    let logs = crate::pool::arrival::logs_of(&source);
    let spoke_pool = address("0x09aea4b2242abc8bb4bb78d537a67a245a7bec64");
    let payer = address("0xf00eba8e5fd231b526adfaaa68b4d0ede72a89ac");
    let DepositRead::One(out) = deposit_in(&logs, spoke_pool, payer, ChainId::BASE) else {
        panic!("one deposit");
    };
    assert_eq!(out.destination, ChainId::ARBITRUM);
    assert_eq!(out.fill_deadline(), 1_794_161_828);
    assert_eq!(out.relay.input_amount, TokenAmount::from(3_000_000u32));
    // the quote's time when the receipt gives none
    assert_eq!(out.deposit_time, 1_794_160_439);
    assert_eq!(
        deposit_in(&logs, spoke_pool, address(RELAYER), ChainId::BASE),
        DepositRead::None
    );
    let twice: Vec<_> = logs.iter().cloned().chain(logs.iter().cloned()).collect();
    assert_eq!(
        deposit_in(&twice, spoke_pool, payer, ChainId::BASE),
        DepositRead::Many
    );
}

#[test]
fn the_fill_logs_are_read_back_far_enough_in_pages() {
    // BNB Chain at 450 ms: 1,200 s back is 5,333 blocks, and 100
    let pages = log_pages(BlockNumber::new(100_000), 10_000, 8_800, 450).unwrap();
    assert_eq!(pages.first().unwrap().0, 100_000 - 5_333 - 100);
    assert_eq!(pages.last().unwrap().1, 100_000);
    assert!(pages.iter().all(|(from, to)| to - from < LOG_PAGE_BLOCKS));
    assert!(pages.windows(2).all(|pair| pair[1].0 == pair[0].1 + 1));
    // read too long after: a human's
    assert_eq!(
        log_pages(BlockNumber::new(10_000_000), 1_000_000, 0, 450),
        None
    );
}

#[test]
fn an_across_refund_credits_the_oldest_prefix_that_fits() {
    let usdc = |micro: u64| TokenAmount::from(micro);
    // oldest fill deadline first, then id
    let owed = [(3, usdc(30), 200), (1, usdc(10), 100), (2, usdc(20), 100)];
    assert_eq!(credited(&owed, usdc(60)), Some(vec![1, 2, 3]));
    // the remainder past the prefix is surplus: what the step's caller books
    assert_eq!(credited(&owed, usdc(35)), Some(vec![1, 2]));
    assert_eq!(credited(&owed, usdc(10)), Some(vec![1]));
    // a stranger's 1 unit while payouts are owed back: held for a controller
    assert_eq!(credited(&owed, usdc(1)), None);
    // nothing owed back: all of it the canister's own
    assert_eq!(credited(&[], usdc(1)), Some(vec![]));
}

/// A refund is not booked while a payout it may pay back waits for its fill read: its deposit
/// read, neither filled nor unfilled read, its fill deadline past, its deposit within it.
#[test]
fn a_refund_waits_for_a_payout_whose_fill_is_not_read() {
    crate::storage::on_fresh_memory(|| {
        crate::storage::init();
        let id = crate::storage::pool::tests::waiting_withdrawal(
            "0x1111111111111111111111111111111111111111".parse().unwrap(),
        );
        refund_waits_for(crate::storage::pool::payout(id).unwrap());
    });
}

fn refund_waits_for(mut payout: EvmPayout) {
    use types::pool::Reclaim;
    let usdc = |micro: u64| TokenAmount::from(micro);
    let out = types::pool::AcrossOut {
        destination: ChainId::BSC,
        relay: types::swapic::across::RelayData {
            depositor: [0x8f; 32],
            recipient: [0x0f; 32],
            exclusive_relayer: [0; 32],
            input_token: [0x83; 32],
            output_token: [0x8a; 32],
            input_amount: usdc(10),
            output_amount: usdc(10),
            origin_chain_id: ChainId::BASE,
            deposit_id: TokenAmount::from(7_u8),
            fill_deadline: 1_500,
            exclusivity_deadline: 0,
            message: Vec::new(),
        },
        deposit_time: 300,
    };
    // no deposit read yet: signed, its receipt not closed
    assert!(!unread_refundable(std::slice::from_ref(&payout), usdc(10), 2_000));
    payout.across = Some(out);
    assert!(unread_refundable(std::slice::from_ref(&payout), usdc(10), 2_000));
    // before its fill deadline, or a refund smaller than its deposit: not its own
    assert!(!unread_refundable(std::slice::from_ref(&payout), usdc(10), 1_499));
    assert!(!unread_refundable(std::slice::from_ref(&payout), usdc(9), 2_000));
    // read unfilled: owed back, credited at once
    payout.reclaim = Some(Reclaim::Pending);
    assert!(!unread_refundable(std::slice::from_ref(&payout), usdc(10), 2_000));
}
