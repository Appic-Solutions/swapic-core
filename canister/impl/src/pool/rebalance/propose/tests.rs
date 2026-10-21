//! Review Focus 1: a proposal can only ever move the move's USDC to the canister's own
//! address on the other chain, whatever the watcher sends. The proposals are a REAL Eco order,
//! the staging canary's (2026-10-18, `pool/pay/samples/eco_base_to_arbitrum.hex`: 5 USDC on
//! Base to USDC on Arbitrum, one `publishAndFund`), rewritten into the move's own: the payer,
//! the ref, the amount, the token in and the reward's, the intent's destination, its route's
//! token and recipient, the reward's creator and prover, the deadlines. Each hostile test
//! rewrites one more field, and says so. The cheapest-wins rule is the fold's
//! (`types::pool::ledger::rebalance`'s tests: a proposal is taken only when cheaper).

use super::*;
use alloy_sol_types::{SolCall, SolValue};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::MoveStatus;
use types::swapic::{eco_writes, publishAndFundCall, transferCall, EcoRoute};
use types::{CctpDomain, GasAmount, Wei};

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn canister() -> EvmAddress {
    address("0x36D34038F04990a58c068e08eD9754e2E3D9bA53")
}

fn stranger() -> EvmAddress {
    address("0x000000000000000000000000000000000000dEaD")
}

fn prover() -> EvmAddress {
    address("0xEC08fb4647f3f50d1162a578d481266687C60fc5")
}

fn portal() -> EvmAddress {
    address("0xEC000769A73b70e16f361a442292500b3BCf4A85")
}

fn base_usdc() -> EvmAddress {
    address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
}

fn ethereum_usdc() -> EvmAddress {
    address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
}

fn chain(usdc: EvmAddress) -> EvmChain {
    EvmChain {
        swapic: address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"),
        runner: address("0x7E345acaB79102ac9588C60B9358Ec4d79438eEe"),
        cctp_receiver: address("0x23718C0a081B8bf00aee86722Af40E175562Ad47"),
        usdc,
        token_messenger: address("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"),
        message_transmitter: address("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"),
        gas_limit: PerAction::all(GasAmount::from(600_000_u64)),
        gas_budget: PerAction::all(Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
    }
}

struct World {
    evm_chains: ChainTable<EvmChain>,
    domains: ChainTable<CctpDomain>,
}

fn world() -> World {
    World {
        evm_chains: ChainTable(BTreeMap::from([
            (ChainId::BASE, chain(base_usdc())),
            (ChainId::ETHEREUM, chain(ethereum_usdc())),
        ])),
        domains: ChainTable(BTreeMap::from([
            (ChainId::ETHEREUM, CctpDomain::new(0)),
            (ChainId::BASE, CctpDomain::new(6)),
        ])),
    }
}

/// The chains a move runs between, Base the pool's.
fn ends(direction: MoveDirection) -> (ChainId, ChainId) {
    super::super::cost::chains(direction, ChainId::BASE)
}

fn usdc_on(chain: ChainId) -> EvmAddress {
    if chain == ChainId::BASE {
        base_usdc()
    } else {
        ethereum_usdc()
    }
}

fn setup(world: &World, direction: MoveDirection) -> Setup<'_> {
    let (src, dst) = ends(direction);
    Setup {
        chains: Chains {
            chain_id: src,
            evm_chains: &world.evm_chains,
            domains: &world.domains,
            canister: canister(),
            eco_portal: Some(portal()),
        },
        swapic: address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"),
        source_usdc: usdc_on(src),
        destination: dst,
        destination_usdc: usdc_on(dst),
        prover: Some(prover()),
        max_gas: 600_000,
        network: TokenAmount::from(50_000_u64),
    }
}

fn now() -> Timestamp {
    Timestamp::from_secs(1_800_000_000).unwrap()
}

const AMOUNT: u64 = 1_000_000_000;

/// Move 7 of $1,000, where a proposal can help, its window a minute from now, its own route
/// costing $1.
fn item(direction: MoveDirection) -> Move {
    let mut item = Move::opened(
        7,
        direction,
        TokenAmount::from(AMOUNT),
        false,
        now(),
        Some(now().checked_add(std::time::Duration::from_secs(60)).unwrap()),
        TokenAmount::from(1_000_000_u64),
    );
    if direction == MoveDirection::IcpToBase {
        item.status = MoveStatus::Landed;
    }
    item
}

/// The canary's Eco order, rewritten into move 7's proposal: Eco keeping `fee`.
fn proposal(direction: MoveDirection, fee: u64) -> Order {
    let (src, dst) = ends(direction);
    let mut order = swapic::decode_order(
        &hex::decode(include_str!("../../pay/samples/eco_base_to_arbitrum.hex").trim()).unwrap(),
    )
    .unwrap();
    let now_s = now().as_secs().get();
    order.payer = swapic::addr(canister());
    order.r#ref = alloy_primitives::FixedBytes(move_ref(7).into_bytes());
    order.tokenIn = swapic::addr(usdc_on(src));
    order.amountIn = U256::from(AMOUNT);
    order.deadline = U256::from(now_s + 600);
    let step = &mut order.steps[0];
    let mut call = publishAndFundCall::abi_decode(&step.data).unwrap();
    call.destination = dst.get();
    let mut route = <EcoRoute as SolValue>::abi_decode(&call.route).unwrap();
    route.tokens[0].token = swapic::addr(usdc_on(dst));
    route.calls[0].target = swapic::addr(usdc_on(dst));
    route.calls[0].data = transferCall {
        to: swapic::addr(canister()),
        amount: U256::ZERO,
    }
    .abi_encode()
    .into();
    route.deadline = now_s + 900;
    route.portal = swapic::addr(portal());
    let route_bytes = SolValue::abi_encode(&route);
    let route_len = route_bytes.len();
    call.route = route_bytes.into();
    call.reward.creator = swapic::addr(canister());
    call.reward.prover = swapic::addr(prover());
    call.reward.deadline = now_s + 7 * 24 * 3_600;
    call.reward.tokens[0].token = swapic::addr(usdc_on(src));
    step.data = call.abi_encode().into();
    step.token = swapic::addr(usdc_on(src));
    step.target = swapic::addr(portal());
    step.min = U256::from(AMOUNT);
    step.writes = eco_writes(
        &EcoArrival::Deliver {
            recipient: canister(),
        },
        route_len,
        U256::from(fee),
    );
    order
}

fn checked(direction: MoveDirection, order: &Order) -> Result<(PinnedOrder, TokenAmount), PoolError> {
    let world = world();
    check(
        &item(direction),
        order,
        &swapic::encode_order(order),
        400_000,
        &setup(&world, direction),
        &RebalanceSettings::default(),
        now(),
    )
}

/// The intent's call, rewritten by `change` (the route's length kept, so the Runner's writes
/// still land where they did).
fn rewritten(mut order: Order, change: impl FnOnce(&mut publishAndFundCall)) -> Order {
    let mut call = publishAndFundCall::abi_decode(&order.steps[0].data).unwrap();
    change(&mut call);
    order.steps[0].data = call.abi_encode().into();
    order
}

fn refused_with(result: Result<(PinnedOrder, TokenAmount), PoolError>, words: &str) {
    match result {
        Err(PoolError::NotAllowed(why)) => {
            assert!(why.contains(words), "refused for {why:?}, not {words:?}")
        }
        other => panic!("taken, or refused otherwise: {other:?}"),
    }
}

#[test]
fn a_valid_eco_proposal_is_taken_in_each_direction() {
    for direction in [MoveDirection::BaseToIcp, MoveDirection::IcpToBase] {
        let order = proposal(direction, 100_000);
        let (pinned, fee) = checked(direction, &order).unwrap();
        let (src, _) = ends(direction);
        assert_eq!(fee, TokenAmount::from(100_000_u64));
        assert_eq!(pinned.gas_limit, 400_000);
        assert_eq!(
            pinned.order_id,
            swapic::order_id(
                &order,
                src,
                address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406")
            )
            .into_bytes()
        );
        assert_eq!(pinned.deadline_s, now().as_secs().get() + 600);
    }
}

#[test]
fn an_order_paying_another_or_more_is_refused() {
    let direction = MoveDirection::BaseToIcp;
    // rewritten: the route delivers to a stranger
    let order = rewritten(proposal(direction, 100_000), |call| {
        let mut route = <EcoRoute as SolValue>::abi_decode(&call.route).unwrap();
        route.calls[0].data = transferCall {
            to: swapic::addr(stranger()),
            amount: U256::ZERO,
        }
        .abi_encode()
        .into();
        call.route = SolValue::abi_encode(&route).into();
    });
    refused_with(checked(direction, &order), "another than the canister");
    // rewritten: more than the move
    let mut more = proposal(direction, 100_000);
    more.amountIn = U256::from(AMOUNT + 1);
    refused_with(checked(direction, &more), "not the move's");
    // rewritten: another payer
    let mut theirs = proposal(direction, 100_000);
    theirs.payer = swapic::addr(stranger());
    refused_with(checked(direction, &theirs), "payer is not the canister");
    // rewritten: another ref
    let mut other_ref = proposal(direction, 100_000);
    other_ref.r#ref = alloy_primitives::FixedBytes(move_ref(8).into_bytes());
    refused_with(checked(direction, &other_ref), "ref is not the move's");
    // rewritten: to another chain than the move's other pocket
    let elsewhere = rewritten(proposal(direction, 100_000), |call| call.destination = 42_161);
    refused_with(checked(direction, &elsewhere), "");
}

#[test]
fn an_intent_another_could_claim_or_refund_is_refused() {
    let direction = MoveDirection::IcpToBase;
    // rewritten: a creator other than the canister, whom an unfilled intent would refund
    let creator = rewritten(proposal(direction, 100_000), |call| {
        call.reward.creator = swapic::addr(stranger())
    });
    // (the reader refuses it first: an intent refunds its order's payer, the canister)
    refused_with(checked(direction, &creator), "refund");
    // rewritten: another prover, who could pay the reward to anyone
    let proven = rewritten(proposal(direction, 100_000), |call| {
        call.reward.prover = swapic::addr(stranger())
    });
    refused_with(checked(direction, &proven), "another prover");
    // with no prover configured, nothing is taken
    let world = world();
    let mut blind = setup(&world, direction);
    blind.prover = None;
    let order = proposal(direction, 100_000);
    refused_with(
        check(
            &item(direction),
            &order,
            &swapic::encode_order(&order),
            400_000,
            &blind,
            &RebalanceSettings::default(),
            now(),
        ),
        "no Eco prover",
    );
}

#[test]
fn a_dex_step_or_a_fee_step_is_refused() {
    let direction = MoveDirection::BaseToIcp;
    // rewritten: a KyberSwap step of the canary's own before the intent
    let swap = swapic::decode_order(
        &hex::decode(include_str!("../../pay/samples/same_chain_base.hex").trim()).unwrap(),
    )
    .unwrap()
    .steps[0]
        .clone();
    let mut dex = proposal(direction, 100_000);
    dex.steps.insert(0, swap);
    assert!(checked(direction, &dex).is_err());
    // rewritten: a fee step to the canister before the intent
    let mut fee_step = proposal(direction, 100_000);
    let fees = swapic::decode_order(
        &hex::decode(include_str!("../../pay/samples/eco_with_fee_step.hex").trim()).unwrap(),
    )
    .unwrap();
    let mut transfer = fees.steps[0].clone();
    transfer.target = swapic::addr(base_usdc());
    transfer.data = transferCall {
        to: swapic::addr(canister()),
        amount: U256::from(50_000_u64),
    }
    .abi_encode()
    .into();
    fee_step.steps.insert(0, transfer);
    refused_with(checked(direction, &fee_step), "no fee step");
}

#[test]
fn a_fee_above_the_cap_or_not_below_the_own_route_is_refused() {
    let direction = MoveDirection::BaseToIcp;
    // 2 bps of $1,000 is 20 cents
    refused_with(
        checked(direction, &proposal(direction, 200_001)),
        "above the cap",
    );
    // the own route costs $1: Eco's 0.95 and the canister's 0.05 are not below it
    let world = world();
    let mut dear = item(direction);
    dear.own_cost = TokenAmount::from(150_000_u64);
    let order = proposal(direction, 100_000);
    refused_with(
        check(
            &dear,
            &order,
            &swapic::encode_order(&order),
            400_000,
            &setup(&world, direction),
            &RebalanceSettings::default(),
            now(),
        ),
        "not below the own route",
    );
}

#[test]
fn outside_the_window_for_an_urgent_move_or_with_long_deadlines_nothing_is_taken() {
    let direction = MoveDirection::BaseToIcp;
    let world = world();
    let order = proposal(direction, 100_000);
    let at = |item: &Move, when: Timestamp, gas: u64| {
        check(
            item,
            &order,
            &swapic::encode_order(&order),
            gas,
            &setup(&world, direction),
            &RebalanceSettings::default(),
            when,
        )
    };
    let late = now().checked_add(std::time::Duration::from_secs(60)).unwrap();
    refused_with(at(&item(direction), late, 400_000), "window is over");
    let mut urgent = item(direction);
    urgent.urgent = true;
    refused_with(at(&urgent, now(), 400_000), "takes no proposal");
    // an ICP to Base move before its USDC landed takes none
    let mut early = item(MoveDirection::IcpToBase);
    early.status = MoveStatus::Withdrawn;
    assert!(checked(MoveDirection::IcpToBase, &proposal(MoveDirection::IcpToBase, 1)).is_ok());
    refused_with(
        check(
            &early,
            &proposal(MoveDirection::IcpToBase, 1),
            &[],
            400_000,
            &setup(&world, MoveDirection::IcpToBase),
            &RebalanceSettings::default(),
            now(),
        ),
        "takes no proposal",
    );
    // more gas than the move's row
    refused_with(at(&item(direction), now(), 600_001), "gas limit");
    // rewritten: an order that may wait an hour to run
    let mut slow = proposal(direction, 100_000);
    slow.deadline = U256::from(now().as_secs().get() + 3_600);
    refused_with(checked(direction, &slow), "deadline");
}
