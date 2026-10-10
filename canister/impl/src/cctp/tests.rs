use super::*;
use crate::storage::events::append_event_at;
use crate::storage::on_fresh_memory;
use alloy_primitives::U256;
use std::collections::BTreeMap;
use std::time::Duration;
use types::config::ChainTable;
use types::events::TxPurpose;
use types::router::ACTION_SWAP;
use types::{CctpDomain, EventType, FlowStep, Nonce, TokenAmount, TxHash, WeiPerGas};

/// A real CCTP v2 fast transfer from Base to Arbitrum, as Circle's iris served it
/// (`samples/cctp_base_to_arbitrum.json`, burn tx 0x90cc...da0c).
fn sample() -> (Vec<u8>, Vec<u8>) {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("../../samples/cctp_base_to_arbitrum.json")).unwrap();
    let hex_of = |field: &str| {
        hex::decode(
            json["iris"][field]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
        )
        .unwrap()
    };
    (hex_of("message"), hex_of("attestation"))
}

const BASE: ChainId = ChainId::BASE;
const ARBITRUM: ChainId = ChainId::ARBITRUM;
const SWAPIC_BASE: EvmAddress = EvmAddress::new([0xb5; 20]);
const SWAPIC_ARB: EvmAddress = EvmAddress::new([0xa5; 20]);
const TRANSMITTER: EvmAddress = EvmAddress::new([
    0x81, 0xd4, 0x0f, 0x21, 0xf1, 0x2a, 0x8f, 0x0e, 0x32, 0x52, 0xbc, 0xcb, 0x95, 0x4d, 0x72, 0x2d,
    0x4c, 0x46, 0x4b, 0x64,
]);
const MESSENGER: EvmAddress = EvmAddress::new([
    0x28, 0xb5, 0xa0, 0xe9, 0xc6, 0x21, 0xa5, 0xba, 0xda, 0xa5, 0x36, 0x21, 0x9b, 0x3a, 0x22, 0x8c,
    0x81, 0x68, 0xcf, 0x5d,
]);
const BASE_USDC: EvmAddress = EvmAddress::new([
    0x83, 0x35, 0x89, 0xfc, 0xd6, 0xed, 0xb6, 0xe0, 0x8f, 0x4c, 0x7c, 0x32, 0xd4, 0xf7, 0x1b, 0x54,
    0xbd, 0xa0, 0x29, 0x13,
]);
const NOW: u64 = 1_790_000_000;

fn usdc() -> ChainTable<EvmAddress> {
    ChainTable(BTreeMap::from([(BASE, BASE_USDC)]))
}

fn domains() -> ChainTable<CctpDomain> {
    ChainTable(BTreeMap::from([
        (ChainId::ETHEREUM, CctpDomain::new(0)),
        (BASE, CctpDomain::new(6)),
        (ARBITRUM, CctpDomain::new(3)),
    ]))
}

fn settings() -> FlowSettings {
    FlowSettings {
        routers: BTreeMap::from([(BASE, SWAPIC_BASE), (ARBITRUM, SWAPIC_ARB)]),
        ethereum: ChainId::ETHEREUM,
        ethereum_usdc: None,
        ckusdc_ledger: None,
        ck_minter: None,
        cketh_ledger: None,
        cketh_fee_allowance: TokenAmount::ZERO,
        icpswap_pools: vec![],
        relay_window: Duration::from_secs(3_600),
        continuation_deadline: Duration::from_secs(1_800),
        max_gas_limit: 2_000_000,
    }
}

fn decide_on(chain_id: ChainId, message: &BurnMessage, now_s: u64) -> Result<Plan, String> {
    let (domains, usdc, settings) = (domains(), usdc(), settings());
    let setup = Setup {
        chain_id,
        domains: &domains,
        usdc: &usdc,
        settings: &settings,
        messenger: Some(MESSENGER),
        transmitter: Some(TRANSMITTER),
    };
    decide(message, &setup, now_s)
}

fn hook(action: u8) -> HookData {
    HookData {
        version: 1,
        reference: [0x11; 32],
        action,
        deadline: NOW + 3_600,
        recipient: EvmAddress::new([0x66; 20]),
        token_out: EvmAddress::new([0x77; 20]),
        min_out: U256::from(500_000),
        principal: [0; 32],
        subaccount: [0; 32],
    }
}

/// The real message re-made as Swapic's own burn: sent by Swapic on Base with its hook
/// data, and for a Swap or a CkDeposit minted to Swapic on Arbitrum and called by it.
fn ours(action: u8) -> BurnMessage {
    let (bytes, _) = sample();
    let mut message = BurnMessage::parse(&bytes).unwrap();
    message.body.message_sender = SWAPIC_BASE.to_word();
    message.body.hook_data = hook(action).encode();
    if action != ACTION_DELIVER {
        message.body.mint_recipient = SWAPIC_ARB.to_word();
        message.destination_caller = SWAPIC_ARB.to_word();
    }
    message
}

#[test]
fn the_real_message_reads_as_circle_attested_it() {
    let (bytes, attestation) = sample();
    let message = BurnMessage::parse(&bytes).unwrap();
    assert_eq!((message.source_domain, message.destination_domain), (6, 3));
    assert_eq!(
        hex::encode(message.nonce),
        "1280877a8b106ebbfa4049f1d4ebc852d530ca8e243a59a2389847418e9bf597"
    );
    assert_eq!(message.destination_caller, [0; 32]);
    assert_eq!(message.sender, MESSENGER.to_word());
    assert_eq!(message.recipient, MESSENGER.to_word());
    assert_eq!(message.body.burn_token, BASE_USDC.to_word());
    assert_eq!(
        hex::encode(&message.body.mint_recipient[12..]),
        "c5a0d52edf2dc3df76ed5d2848088270fcf6e2a4"
    );
    assert_eq!(message.body.amount, TokenAmount::from(600_000_u32));
    assert!(message.body.hook_data.is_empty());
    assert_eq!(message.encode(), bytes);
    assert_eq!(attestation.len(), 130, "two attesters' signatures");
}

#[test]
fn a_burn_swapic_did_not_make_is_refused() {
    let (bytes, _) = sample();
    let foreign = BurnMessage::parse(&bytes).unwrap();
    assert_eq!(
        decide_on(ARBITRUM, &foreign, NOW),
        Err("the burn was not made by Swapic".into())
    );
    let mut no_hook = ours(ACTION_DELIVER);
    no_hook.body.hook_data.clear();
    assert_eq!(
        decide_on(ARBITRUM, &no_hook, NOW),
        Err("the message carries no Swapic hook data".into())
    );
}

/// CCTP's generic messaging is permissionless: anyone can have a body shaped like a
/// Swapic burn attested, with the outer sender being their own contract.
#[test]
fn forged_generic_message_is_refused() {
    let mut forged = ours(ACTION_DELIVER);
    forged.sender = [0x0f; 32];
    assert_eq!(
        decide_on(ARBITRUM, &forged, NOW),
        Err("the message is not a token messenger burn".into())
    );
}

#[test]
fn wrong_outer_recipient_is_refused() {
    let mut forged = ours(ACTION_DELIVER);
    forged.recipient = [0x0f; 32];
    assert_eq!(
        decide_on(ARBITRUM, &forged, NOW),
        Err("the message is not a token messenger burn".into())
    );
    let mut not_usdc = ours(ACTION_DELIVER);
    not_usdc.body.burn_token = [0x0e; 32];
    assert_eq!(
        decide_on(ARBITRUM, &not_usdc, NOW),
        Err("the burn is not USDC".into())
    );
}

#[test]
fn a_deliver_is_received_on_the_transmitter() {
    let plan = decide_on(ARBITRUM, &ours(ACTION_DELIVER), NOW).unwrap();
    assert_eq!(plan.action, FinishAction::Receive);
    assert_eq!(plan.to, TRANSMITTER);
    // even after its deadline: a Deliver mints to the user whenever it is received
    let late = decide_on(ARBITRUM, &ours(ACTION_DELIVER), NOW + 7_200).unwrap();
    assert_eq!(late.action, FinishAction::Receive);
    let mut called = ours(ACTION_DELIVER);
    called.destination_caller = SWAPIC_ARB.to_word();
    assert!(decide_on(ARBITRUM, &called, NOW).is_err());
}

#[test]
fn a_swap_before_its_deadline_is_completed_and_after_it_released() {
    let plan = decide_on(ARBITRUM, &ours(ACTION_SWAP), NOW).unwrap();
    assert_eq!((plan.action, plan.to), (FinishAction::Complete, SWAPIC_ARB));
    let late = decide_on(ARBITRUM, &ours(ACTION_SWAP), NOW + 3_600).unwrap();
    assert_eq!((late.action, late.to), (FinishAction::Release, SWAPIC_ARB));
    let fresh =
        completion_swap(&plan.hook, Some((EvmAddress::new([0x44; 20]), vec![1, 2]))).unwrap();
    assert_eq!(fresh.target, router::addr(EvmAddress::new([0x44; 20])));
}

#[test]
fn a_ck_deposit_is_completed_with_no_swap() {
    let plan = decide_on(ARBITRUM, &ours(ACTION_CK_DEPOSIT), NOW).unwrap();
    assert_eq!(plan.action, FinishAction::Complete);
    assert_eq!(
        completion_swap(&plan.hook, None).unwrap().target,
        Address::ZERO
    );
    assert!(completion_swap(&plan.hook, Some((EvmAddress::new([0x44; 20]), vec![]))).is_err());
}

#[test]
fn a_message_for_another_chain_or_minted_elsewhere_is_refused() {
    assert!(decide_on(BASE, &ours(ACTION_DELIVER), NOW).is_err());
    let mut elsewhere = ours(ACTION_SWAP);
    elsewhere.body.mint_recipient = [0x99; 32];
    assert!(decide_on(ARBITRUM, &elsewhere, NOW).is_err());
    let mut anyone = ours(ACTION_SWAP);
    anyone.destination_caller = [0; 32];
    assert!(decide_on(ARBITRUM, &anyone, NOW).is_err());
}

#[test]
fn each_action_calls_what_it_names() {
    let (message, attestation) = sample();
    let none = completion_swap(&hook(ACTION_SWAP), None).unwrap();
    // receiveMessage(bytes,bytes), completeCctp(bytes,bytes,(address,bytes)), release(bytes,bytes)
    for (action, selector) in [
        (FinishAction::Receive, "57ecfd28"),
        (FinishAction::Complete, "5072459f"),
        (FinishAction::Release, "aa30d53a"),
    ] {
        let data = calldata(action, &message, &attestation, none.clone());
        assert_eq!(hex::encode(&data[..4]), selector);
    }
}

fn created(key: QuoteHash, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose: TxPurpose::Finish(key),
        chain_id: ARBITRUM,
        nonce: Nonce::new(nonce),
        to: TRANSMITTER,
        value: Wei::ZERO,
        data: vec![0x57, 0xec, 0xfd, 0x28],
        gas_limit: GasAmount::from(200_000_u32),
        max_fee: WeiPerGas::from(20_000_000_u64),
        max_priority_fee: WeiPerGas::from(1_000_000_u64),
    }
}

fn signed(key: QuoteHash, nonce: u64, action: FinishAction, message_hash: [u8; 32]) -> EventType {
    EventType::Flow {
        quote_hash: key,
        step: FlowStep::CctpFinishSigned {
            chain_id: ARBITRUM,
            nonce: Nonce::new(nonce),
            tx_hash: TxHash::new([nonce as u8 + 1; 32]),
            raw_tx: vec![0x02, 0xf8],
            action,
            message_hash,
        },
    }
}

/// Review Focus 2: the same nudge twice signs once. The fold refuses a second nonce while
/// the first is being signed and a second record once it is signed, and the door answers
/// the first record's id.
#[test]
fn finish_cctp_is_idempotent() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let (message, _) = sample();
        let message_hash = alloy_primitives::keccak256(&message).0;
        let key = finish_key(message_hash, FinishAction::Receive);
        assert_eq!(existing(key), Ok(None));

        append_event_at(created(key, 0), at).unwrap();
        assert_eq!(existing(key), Err("the finish is being signed".into()));
        assert!(
            append_event_at(created(key, 1), at).is_err(),
            "no second nonce while the first is out"
        );

        let id = append_event_at(signed(key, 0, FinishAction::Receive, message_hash), at)
            .unwrap()
            .get();
        assert_eq!(existing(key), Ok(Some(id)));
        assert!(
            append_event_at(created(key, 1), at).is_err(),
            "no second transaction once one is signed"
        );
        assert!(append_event_at(signed(key, 0, FinishAction::Receive, message_hash), at).is_err());
        let state = read_state(|state| state.flow(&key)).unwrap();
        assert_eq!(state.status, types::FlowStatus::FinishSigned);
        assert!(state.tx_open);

        // a record under a key that is not its message and action's is refused
        let release = finish_key(message_hash, FinishAction::Release);
        append_event_at(created(release, 1), at).unwrap();
        assert!(
            append_event_at(signed(release, 1, FinishAction::Receive, message_hash), at).is_err()
        );
        // and the release of the same message is its own action, signed once too
        let release_id =
            append_event_at(signed(release, 1, FinishAction::Release, message_hash), at)
                .unwrap()
                .get();
        assert_eq!(existing(release), Ok(Some(release_id)));
    });
}
