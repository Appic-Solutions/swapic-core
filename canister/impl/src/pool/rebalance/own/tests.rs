//! The canister's own orders, decoded step by step against a REAL CCTP order: the staging
//! canary's Ethereum to Base CCTP fast order (2026-10-19, mainnet, `samples/eth_cctp_to_base.hex`,
//! user pays: a fee step, then the burn). An ICP to Base move's order on Ethereum is that
//! order's shape exactly; it must decode identically but for the amount the Runner writes,
//! the ref and the recipient (and with them the envelope's deadline and least amount, and
//! the step's `min`), and it carries no fee step. A Base to ICP move's order is read back by
//! the canister's own reader (`relay::read_plan`) as the CkDeposit the CctpReceiver completes.

use super::*;
use crate::relay::{read_plan, Chains, Party};
use alloy_sol_types::SolCall;
use std::collections::BTreeMap;
use types::config::{ChainTable, PerAction};
use types::{CctpDomain, ChainId, GasAmount, Wei};

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn canister() -> EvmAddress {
    address("0x36D34038F04990a58c068e08eD9754e2E3D9bA53")
}

fn principal() -> Principal {
    Principal::from_text("2ibo7-dia").unwrap()
}

/// The staging deploy: one Swapic, Runner and CctpReceiver address on every chain, CCTP v2's
/// one TokenMessengerV2 and MessageTransmitterV2.
fn chain(usdc: &str) -> EvmChain {
    EvmChain {
        swapic: address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"),
        runner: address("0x7E345acaB79102ac9588C60B9358Ec4d79438eEe"),
        cctp_receiver: address("0x23718C0a081B8bf00aee86722Af40E175562Ad47"),
        usdc: address(usdc),
        token_messenger: address("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"),
        message_transmitter: address("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"),
        gas_limit: PerAction::all(GasAmount::from(500_000_u64)),
        gas_budget: PerAction::all(Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
        usdc_decimals: None,
        across: None,
    }
}

fn ethereum() -> EvmChain {
    chain("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
}

fn base() -> EvmChain {
    chain("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
}

fn canary() -> Order {
    swapic::decode_order(
        &hex::decode(include_str!("../samples/eth_cctp_to_base.hex").trim()).unwrap(),
    )
    .unwrap()
}

/// The canary's own inputs: 4.9 USDC, fast, Circle offered 970 at the most.
const AMOUNT: u64 = 4_900_000;
const MAX_FEE: u64 = 970;
const NOW_S: u64 = 1_792_402_000;

fn icp_to_base() -> Order {
    let (source, destination) = (ethereum(), base());
    OwnRoute {
        direction: MoveDirection::IcpToBase,
        source: &source,
        destination: &destination,
        destination_domain: 6,
        canister: canister(),
        principal: principal(),
    }
    .order(
        1,
        TokenAmount::from(AMOUNT),
        TokenAmount::from(MAX_FEE),
        true,
        NOW_S,
    )
}

fn burn_call(step: &Step) -> depositForBurnWithHookCall {
    depositForBurnWithHookCall::abi_decode(&step.data).unwrap()
}

#[test]
fn an_icp_to_base_order_decodes_as_the_canarys_burn_but_amount_ref_and_recipient() {
    let theirs = canary();
    let ours = icp_to_base();
    // the order: the canary's token in and amount, no payout on the source; ours carries no
    // fee step, and is the canister's, under the move's ref
    assert_eq!(ours.tokenIn, theirs.tokenIn);
    assert_eq!(ours.amountIn, theirs.amountIn);
    assert_eq!(ours.payout, theirs.payout);
    assert_eq!(
        theirs.steps.len(),
        2,
        "the canary's fee step, then its burn"
    );
    assert_eq!(ours.steps.len(), 1, "no fee step");
    assert_eq!(ours.payer, swapic::addr(canister()));
    assert_eq!(ours.r#ref.0, move_ref(1).into_bytes());

    // the burn step, field by field
    let (mine, canarys) = (&ours.steps[0], &theirs.steps[1]);
    assert_eq!(mine.target, canarys.target, "TokenMessengerV2");
    assert_eq!(mine.token, canarys.token);
    assert_eq!(
        mine.writes, canarys.writes,
        "the Runner writes the amount at 4"
    );
    // the step's min is the delta the Runner gives it: the whole amount with no fee step
    assert_eq!(mine.min, U256::from(AMOUNT));
    let (a, b) = (burn_call(mine), burn_call(canarys));
    assert_eq!(a.amount, U256::ZERO);
    assert_eq!(a.amount, b.amount);
    assert_eq!(a.destinationDomain, b.destinationDomain, "Base");
    assert_eq!(a.burnToken, b.burnToken);
    assert_eq!(
        a.destinationCaller, b.destinationCaller,
        "received by anyone"
    );
    assert_eq!(a.maxFee, b.maxFee);
    assert_eq!(a.minFinalityThreshold, b.minFinalityThreshold, "fast: 1000");
    // the recipient: the canister's own Base address
    assert_eq!(a.mintRecipient.0, canister().to_word());
    assert_ne!(a.mintRecipient, b.mintRecipient);

    // the hook: the v3 envelope, the same but for the ref, the recipient, its deadline and
    // its least amount
    let (mine, canarys) = (
        Envelope::decode(&a.hookData).unwrap(),
        Envelope::decode(&b.hookData).unwrap(),
    );
    assert_eq!(mine.action, canarys.action, "Deliver");
    assert_eq!(mine.token_out, canarys.token_out, "Base's USDC");
    assert_eq!(mine.principal, canarys.principal);
    assert_eq!(mine.subaccount, canarys.subaccount);
    assert_eq!(mine.min_gas, canarys.min_gas);
    assert_eq!(mine.reference, move_ref(1).into_bytes());
    assert_eq!(mine.recipient, canister());
    assert_eq!(mine.min_out, U256::from(AMOUNT - MAX_FEE));
    assert_eq!(mine.deadline, NOW_S + ORDER_LIFE_S + ENVELOPE_LIFE_S);
}

#[test]
fn standard_finality_is_2000() {
    let (source, destination) = (ethereum(), base());
    let order = OwnRoute {
        direction: MoveDirection::IcpToBase,
        source: &source,
        destination: &destination,
        destination_domain: 6,
        canister: canister(),
        principal: principal(),
    }
    .order(
        1,
        TokenAmount::from(AMOUNT),
        TokenAmount::ZERO,
        false,
        NOW_S,
    );
    let call = burn_call(&order.steps[0]);
    assert_eq!(call.minFinalityThreshold, STANDARD_FINALITY);
    assert_eq!(call.maxFee, U256::ZERO);
}

fn domains() -> ChainTable<CctpDomain> {
    ChainTable(BTreeMap::from([
        (ChainId::ETHEREUM, CctpDomain::new(0)),
        (ChainId::BASE, CctpDomain::new(6)),
    ]))
}

fn chains() -> ChainTable<EvmChain> {
    ChainTable(BTreeMap::from([
        (ChainId::ETHEREUM, ethereum()),
        (ChainId::BASE, base()),
    ]))
}

/// The canister reads its own orders as the Runner will run them: an ICP to Base order
/// delivers Base's USDC to the canister, at least the amount less Circle's most.
#[test]
fn the_canisters_reader_reads_an_icp_to_base_order_as_a_delivery_to_itself() {
    let (evm_chains, domains) = (chains(), domains());
    let plan = read_plan(
        &icp_to_base(),
        &Chains {
            chain_id: ChainId::ETHEREUM,
            evm_chains: &evm_chains,
            domains: &domains,
            canister: canister(),
            eco_portal: None,
            across_fee: None,
        },
    )
    .unwrap();
    assert!(plan.fees.is_empty());
    let delivery = plan.delivery.unwrap();
    assert_eq!(delivery.chain, ChainId::BASE);
    assert_eq!(delivery.recipient, canister());
    assert_eq!(delivery.token, base().usdc);
    assert_eq!(delivery.min_out, TokenAmount::from(AMOUNT - MAX_FEE));
}

/// A Base to ICP order is a CkDeposit for the canister's OWN principal and rebalance
/// subaccount, minted to and called by Ethereum's CctpReceiver, refunding the canister.
#[test]
fn a_base_to_icp_order_is_a_ck_deposit_for_the_canister_itself() {
    let (source, destination) = (base(), ethereum());
    let order = OwnRoute {
        direction: MoveDirection::BaseToIcp,
        source: &source,
        destination: &destination,
        destination_domain: 0,
        canister: canister(),
        principal: principal(),
    }
    .order(
        2,
        TokenAmount::from(1_000_000_000_u64),
        TokenAmount::ZERO,
        false,
        NOW_S,
    );
    let (evm_chains, domains) = (chains(), domains());
    let plan = read_plan(
        &order,
        &Chains {
            chain_id: ChainId::BASE,
            evm_chains: &evm_chains,
            domains: &domains,
            canister: canister(),
            eco_portal: None,
            across_fee: None,
        },
    )
    .unwrap();
    assert!(plan.fees.is_empty());
    let arrival = plan.arrival.unwrap();
    assert_eq!(arrival.chain_id, ChainId::ETHEREUM);
    assert_eq!(arrival.action, types::config::GasAction::CompleteCk);
    assert_eq!(arrival.recipient, canister());
    assert!(arrival.parties.contains(&Party::Icp(principal())));
    let call = burn_call(&order.steps[0]);
    assert_eq!(call.destinationDomain, 0);
    assert_eq!(call.mintRecipient.0, ethereum().cctp_receiver.to_word());
    assert_eq!(call.destinationCaller.0, ethereum().cctp_receiver.to_word());
    assert_eq!(call.burnToken, swapic::addr(base().usdc));
    let envelope = Envelope::decode(&call.hookData).unwrap();
    assert_eq!(envelope.action, Action::CkDeposit);
    assert_eq!(swapic::ck_principal(envelope.principal), Some(principal()));
    assert_eq!(envelope.subaccount, rebalance_subaccount());
    assert_eq!(envelope.reference, move_ref(2).into_bytes());
    assert_eq!(envelope.min_gas, MIN_GAS_CK_DEPOSIT);
    assert_eq!(order.amountIn, U256::from(1_000_000_000_u64));
    assert_eq!(order.deadline, U256::from(NOW_S + ORDER_LIFE_S));
}
