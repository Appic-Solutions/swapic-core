use super::*;
use crate::storage::events::append_event_at;
use crate::storage::on_fresh_memory;
use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use std::collections::BTreeMap;
use types::config::PerAction;
use types::{EventType, FlowStep, GasAmount, Nonce, TokenAmount, WeiPerGas};

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
const BASE_RUNNER: EvmAddress = EvmAddress::new([0xb6; 20]);
const ARB_RECEIVER: EvmAddress = EvmAddress::new([0xa7; 20]);
const CANISTER: EvmAddress = EvmAddress::new([0xca; 20]);
const USER: EvmAddress = EvmAddress::new([0x66; 20]);
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
const DEADLINE: u64 = NOW + 3_600;

fn chain(swapic: u8, runner: EvmAddress, receiver: EvmAddress, usdc: EvmAddress) -> EvmChain {
    EvmChain {
        swapic: EvmAddress::new([swapic; 20]),
        runner,
        cctp_receiver: receiver,
        usdc,
        token_messenger: MESSENGER,
        message_transmitter: TRANSMITTER,
        gas_limit: PerAction::all(GasAmount::from(1_500_000_u64)),
        gas_budget: PerAction::all(Wei::from(10_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
    }
}

fn evm_chains() -> ChainTable<EvmChain> {
    ChainTable(BTreeMap::from([
        (
            BASE,
            chain(0xb5, BASE_RUNNER, EvmAddress::new([0xb7; 20]), BASE_USDC),
        ),
        (
            ARBITRUM,
            chain(
                0xa5,
                EvmAddress::new([0xa6; 20]),
                ARB_RECEIVER,
                EvmAddress::new([0xaf; 20]),
            ),
        ),
    ]))
}

fn domains() -> ChainTable<CctpDomain> {
    ChainTable(BTreeMap::from([
        (ChainId::ETHEREUM, CctpDomain::new(0)),
        (BASE, CctpDomain::new(6)),
        (ARBITRUM, CctpDomain::new(3)),
    ]))
}

fn decide_on(chain_id: ChainId, message: &BurnMessage, now_s: u64) -> Result<Plan, FinishError> {
    let (evm_chains, domains) = (evm_chains(), domains());
    let setup = Setup {
        chain_id,
        evm_chains: &evm_chains,
        domains: &domains,
        canister: CANISTER,
    };
    decide(message, &setup, now_s)
}

/// A principal in the ck helper's encoding: its length, then its bytes.
fn principal_word() -> [u8; 32] {
    let mut word = [0_u8; 32];
    word[0] = 10;
    word[1..11].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 1, 1]);
    word
}

fn envelope(action: Action) -> Envelope {
    Envelope {
        reference: [0x11; 32],
        action,
        deadline: DEADLINE,
        recipient: USER,
        token_out: EvmAddress::new([0x77; 20]),
        min_out: U256::from(500_000),
        principal: if action == Action::CkDeposit {
            principal_word()
        } else {
            [0; 32]
        },
        subaccount: [0; 32],
        min_gas: if action == Action::Deliver {
            0
        } else {
            700_000
        },
    }
}

/// The real message re-made as Swapic's own burn: burned by the Runner on Base with a v3
/// envelope, and minted and called as the envelope's action needs on Arbitrum.
fn ours(action: Action) -> BurnMessage {
    let (bytes, _) = sample();
    let mut message = BurnMessage::parse(&bytes).unwrap();
    message.body.message_sender = BASE_RUNNER.to_word();
    message.body.hook_data = envelope(action).encode();
    if action == Action::Deliver {
        message.body.mint_recipient = USER.to_word();
    } else {
        message.body.mint_recipient = ARB_RECEIVER.to_word();
        message.destination_caller = ARB_RECEIVER.to_word();
    }
    message
}

#[test]
fn the_real_message_reads_as_circle_attested_it() {
    let (bytes, attestation) = sample();
    let message = BurnMessage::parse(&bytes).unwrap();
    assert_eq!((message.source_domain, message.destination_domain), (6, 3));
    assert_eq!(message.sender, MESSENGER.to_word());
    assert_eq!(message.recipient, MESSENGER.to_word());
    assert_eq!(message.body.burn_token, BASE_USDC.to_word());
    assert_eq!(message.encode(), bytes);
    assert_eq!(attestation.len(), 130, "two attesters' signatures");
    // re-made as ours, it is exactly the 696 bytes a Swapic message is
    assert_eq!(
        ours(Action::Swap).encode().len(),
        Envelope::CCTP_MESSAGE_LENGTH
    );
}

#[test]
fn deliver_relays() {
    let plan = decide_on(ARBITRUM, &ours(Action::Deliver), NOW).unwrap();
    assert_eq!(
        (plan.action, plan.gas_action, plan.to),
        (FinishAction::Receive, GasAction::RelayDeliver, TRANSMITTER)
    );
    // minted to the user whenever it is received, after the deadline too
    let late = decide_on(ARBITRUM, &ours(Action::Deliver), DEADLINE + 7_200).unwrap();
    assert_eq!(late.action, FinishAction::Receive);
    // received by the canister only is fine; by anyone else it is not ours to relay
    let mut ours_only = ours(Action::Deliver);
    ours_only.destination_caller = CANISTER.to_word();
    assert!(decide_on(ARBITRUM, &ours_only, NOW).is_ok());
    let mut theirs = ours(Action::Deliver);
    theirs.destination_caller = ARB_RECEIVER.to_word();
    assert!(matches!(
        decide_on(ARBITRUM, &theirs, NOW),
        Err(FinishError::Shape(_))
    ));
    // minted to someone other than the envelope names
    let mut elsewhere = ours(Action::Deliver);
    elsewhere.body.mint_recipient = [0x99; 32];
    assert!(matches!(
        decide_on(ARBITRUM, &elsewhere, NOW),
        Err(FinishError::Shape(_))
    ));
    let (message, attestation) = sample();
    let data = calldata(plan.action, &message, &attestation, swapic::no_recipe());
    let call = swapic::receiveMessageCall::abi_decode(&data).unwrap();
    assert_eq!(call.message.to_vec(), message);
}

#[test]
fn swap_before_deadline_completes() {
    let plan = decide_on(ARBITRUM, &ours(Action::Swap), NOW).unwrap();
    assert_eq!(
        (plan.action, plan.gas_action, plan.to),
        (
            FinishAction::Complete,
            GasAction::CompleteSwap,
            ARB_RECEIVER
        )
    );
    assert_eq!(recipe_for(&plan, None), Err(FinishError::NoRecipe));
    let given = GivenRecipe {
        target: EvmAddress::new([0x44; 20]),
        data: vec![0xe2, 0x1f, 0xd0, 0xe9],
        scaler: Some(EvmAddress::new([0x2f; 20])),
    };
    let recipe = recipe_for(&plan, Some(given)).unwrap();
    let message = ours(Action::Swap).encode();
    let data = calldata(plan.action, &message, &[7; 130], recipe.clone());
    assert_eq!(data[..4], swapic::selectors::COMPLETE_CCTP);
    let call = swapic::completeCctpCall::abi_decode(&data).unwrap();
    assert_eq!(
        (
            call.message.to_vec(),
            call.attestation.to_vec(),
            call.recipe
        ),
        (message, vec![7; 130], recipe)
    );
}

#[test]
fn a_ck_deposit_completes_with_no_recipe() {
    let plan = decide_on(ARBITRUM, &ours(Action::CkDeposit), NOW).unwrap();
    assert_eq!(
        (plan.action, plan.gas_action),
        (FinishAction::Complete, GasAction::CompleteCk)
    );
    assert_eq!(recipe_for(&plan, None), Ok(swapic::no_recipe()));
    let given = GivenRecipe {
        target: EvmAddress::new([0x44; 20]),
        data: vec![],
        scaler: None,
    };
    assert_eq!(
        recipe_for(&plan, Some(given)),
        Err(FinishError::RecipeForCk)
    );
}

#[test]
fn after_deadline_releases() {
    let plan = decide_on(
        ARBITRUM,
        &ours(Action::Swap),
        DEADLINE + RELEASE_MARGIN_S + 1,
    )
    .unwrap();
    assert_eq!(
        (plan.action, plan.gas_action, plan.to),
        (FinishAction::Release, GasAction::Release, ARB_RECEIVER)
    );
    let message = ours(Action::Swap).encode();
    let data = calldata(plan.action, &message, &[7; 130], swapic::no_recipe());
    assert_eq!(data[..4], swapic::selectors::RELEASE);
    // too close to the deadline either way: nothing is signed, and the watcher asks again
    for now in [
        DEADLINE - COMPLETE_MARGIN_S,
        DEADLINE,
        DEADLINE + RELEASE_MARGIN_S,
    ] {
        assert!(matches!(
            decide_on(ARBITRUM, &ours(Action::Swap), now),
            Err(FinishError::NearDeadline { .. })
        ));
    }
    assert_eq!(
        decide_on(ARBITRUM, &ours(Action::CkDeposit), DEADLINE + 3_600)
            .unwrap()
            .action,
        FinishAction::Release
    );
}

/// A Swap or a CkDeposit is the receiver's to claim: minted to it and called only by it,
/// or the receiver refuses the message. And the message is a Swapic burn end to end.
#[test]
fn wrong_receiver_refused() {
    let mut elsewhere = ours(Action::Swap);
    elsewhere.body.mint_recipient = [0x99; 32];
    assert!(matches!(
        decide_on(ARBITRUM, &elsewhere, NOW),
        Err(FinishError::Shape(_))
    ));
    let mut anyone = ours(Action::Swap);
    anyone.destination_caller = [0; 32];
    assert!(matches!(
        decide_on(ARBITRUM, &anyone, NOW),
        Err(FinishError::Shape(_))
    ));
    // CCTP's generic messaging is permissionless: a body shaped like a burn, sent by
    // someone's own contract, is not a token messenger burn
    let mut forged = ours(Action::Swap);
    forged.sender = [0x0f; 32];
    assert_eq!(
        decide_on(ARBITRUM, &forged, NOW),
        Err(FinishError::NotABurn)
    );
    let mut wrong_recipient = ours(Action::Swap);
    wrong_recipient.recipient = [0x0f; 32];
    assert_eq!(
        decide_on(ARBITRUM, &wrong_recipient, NOW),
        Err(FinishError::NotABurn)
    );
    let mut not_usdc = ours(Action::Swap);
    not_usdc.body.burn_token = [0x0e; 32];
    assert_eq!(
        decide_on(ARBITRUM, &not_usdc, NOW),
        Err(FinishError::NotUsdc)
    );
    let (bytes, _) = sample();
    let mut not_ours = BurnMessage::parse(&bytes).unwrap();
    not_ours.body.hook_data = envelope(Action::Swap).encode();
    assert_eq!(
        decide_on(ARBITRUM, &not_ours, NOW),
        Err(FinishError::NotTheRunner)
    );
    let mut no_envelope = ours(Action::Swap);
    no_envelope.body.hook_data.clear();
    assert!(matches!(
        decide_on(ARBITRUM, &no_envelope, NOW),
        Err(FinishError::Envelope(_))
    ));
    assert!(matches!(
        decide_on(BASE, &ours(Action::Swap), NOW),
        Err(FinishError::WrongDomain { .. })
    ));
}

fn created(key: QuoteHash, nonce: u64) -> EventType {
    EventType::TxCreated {
        purpose: TxPurpose::Finish(key),
        chain_id: ARBITRUM,
        nonce: Nonce::new(nonce),
        to: ARB_RECEIVER,
        value: Wei::ZERO,
        data: swapic::selectors::COMPLETE_CCTP.to_vec(),
        gas_limit: GasAmount::from(1_185_547_u32),
        max_fee: WeiPerGas::from(20_000_000_u64),
        max_priority_fee: WeiPerGas::from(1_000_000_u64),
        action: None,
    }
}

fn signed(nonce: u64, action: FinishAction, arrival: [u8; 32], source_tx: TxHash) -> EventType {
    EventType::Flow {
        quote_hash: finish_key(arrival, action),
        step: FlowStep::CctpFinishSigned {
            chain_id: ARBITRUM,
            nonce: Nonce::new(nonce),
            tx_hash: TxHash::new([nonce as u8 + 1; 32]),
            raw_tx: vec![0x02, 0xf8],
            action,
            arrival,
            source_tx: Some(source_tx),
        },
    }
}

/// Plan Review Focus: the same nudge twice signs once, and a release after the deadline
/// answers the completion unless the completion reverted.
#[test]
fn finish_cctp_is_idempotent() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let arrival = alloy_primitives::keccak256(ours(Action::Swap).encode()).0;
        let source = TxHash::new([0x5c; 32]);
        let complete = finish_key(arrival, FinishAction::Complete);
        assert_eq!(answered(FinishAction::Complete, arrival), Ok(None));

        append_event_at(created(complete, 0), at).unwrap();
        assert!(
            answered(FinishAction::Complete, arrival).is_err(),
            "being signed"
        );
        assert!(
            append_event_at(created(complete, 1), at).is_err(),
            "no second nonce while the first is out"
        );
        let id = append_event_at(signed(0, FinishAction::Complete, arrival, source), at)
            .unwrap()
            .get();
        assert_eq!(answered(FinishAction::Complete, arrival), Ok(Some(id)));
        assert!(append_event_at(created(complete, 1), at).is_err());
        // after the deadline the watcher asks again: the completion is out, so it answers
        assert_eq!(answered(FinishAction::Release, arrival), Ok(Some(id)));

        // the completion reverted: a release is what follows it, signed once as well
        append_event_at(
            EventType::Flow {
                quote_hash: complete,
                step: FlowStep::TxClosed {
                    tx_hash: TxHash::new([1; 32]),
                    block: types::BlockNumber::new(9),
                    success: false,
                },
            },
            at,
        )
        .unwrap();
        assert_eq!(answered(FinishAction::Release, arrival), Ok(None));
        let release = finish_key(arrival, FinishAction::Release);
        append_event_at(created(release, 1), at).unwrap();
        let release_id = append_event_at(signed(1, FinishAction::Release, arrival, source), at)
            .unwrap()
            .get();
        assert_eq!(
            answered(FinishAction::Release, arrival),
            Ok(Some(release_id))
        );
    });
}

/// Plan Review Focus: one fee pays for one arrival, so a source transaction finishes one
/// message (its completion and the release after it count as one), and the fold refuses a
/// second message even if a door let it through.
#[test]
fn completions_capped_per_source_tx() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let source = TxHash::new([0x5c; 32]);
        let first = alloy_primitives::keccak256(ours(Action::Swap).encode()).0;
        let second = alloy_primitives::keccak256(ours(Action::CkDeposit).encode()).0;
        append_event_at(created(finish_key(first, FinishAction::Complete), 0), at).unwrap();
        append_event_at(signed(0, FinishAction::Complete, first, source), at).unwrap();

        assert!(read_state(|state| finished_another(state, source, second)));
        assert!(!read_state(|state| finished_another(state, source, first)));
        assert!(!read_state(|state| finished_another(
            state,
            TxHash::new([0x5d; 32]),
            second
        )));

        // the fold, past the door: a second message under the same source is refused
        let second_key = finish_key(second, FinishAction::Complete);
        append_event_at(created(second_key, 1), at).unwrap();
        assert!(append_event_at(signed(1, FinishAction::Complete, second, source), at).is_err());
        // under its own source it is a finish like any other
        append_event_at(
            signed(1, FinishAction::Complete, second, TxHash::new([0x5d; 32])),
            at,
        )
        .unwrap();
        // and the first message's release shares its source's one slot
        let release = finish_key(first, FinishAction::Release);
        append_event_at(created(release, 2), at).unwrap();
        append_event_at(signed(2, FinishAction::Release, first, source), at).unwrap();
    });
}

fn sanctioned(text: String) {
    crate::storage::sanctions::apply(&[text.parse().unwrap()], &[]).unwrap();
}

/// A finish pays its envelope's recipient (and a Deliver its mint recipient, which is
/// the same address): a sanctioned one is not finished.
#[test]
fn a_sanctioned_recipient_is_not_finished() {
    on_fresh_memory(|| {
        crate::storage::init();
        let swap = decide_on(ARBITRUM, &ours(Action::Swap), NOW).unwrap();
        let deliver = decide_on(ARBITRUM, &ours(Action::Deliver), NOW).unwrap();
        assert_eq!(screen(&swap), Ok(()));
        assert_eq!(screen(&deliver), Ok(()));
        sanctioned(USER.to_string());
        assert_eq!(
            screen(&swap),
            Err(FinishError::Sanctioned(Party::Evm(USER)))
        );
        assert_eq!(
            screen(&deliver),
            Err(FinishError::Sanctioned(Party::Evm(USER)))
        );
    });
}

/// A CkDeposit credits an ICP principal: a sanctioned one, or one the canister cannot
/// read, is not finished.
#[test]
fn a_sanctioned_ck_principal_is_not_finished() {
    on_fresh_memory(|| {
        crate::storage::init();
        let plan = decide_on(ARBITRUM, &ours(Action::CkDeposit), NOW).unwrap();
        assert_eq!(screen(&plan), Ok(()));
        let principal = swapic::ck_principal(principal_word()).unwrap();
        sanctioned(principal.to_text());
        assert_eq!(
            screen(&plan),
            Err(FinishError::Sanctioned(Party::Icp(principal)))
        );
        let mut unreadable = ours(Action::CkDeposit);
        let mut bad = envelope(Action::CkDeposit);
        bad.principal = [0xff; 32];
        unreadable.body.hook_data = bad.encode();
        let plan = decide_on(ARBITRUM, &unreadable, NOW).unwrap();
        assert!(matches!(screen(&plan), Err(FinishError::Shape(_))));
    });
}

/// Review fix 8: a finish is keyed by the burn, not by the message's bytes. Circle
/// rewrites finality executed, fee executed and expiration when it attests again, and the
/// re-attested message must be the same arrival: a release after a failed completion of
/// the first attestation is that arrival's release, and a second completion is none.
#[test]
fn a_re_attested_message_is_the_same_arrival() {
    let first = ours(Action::Swap);
    let mut again = first.clone();
    again.finality_threshold_executed = 1_000;
    again.body.fee_executed = TokenAmount::from(1_234_u32);
    again.body.expiration_block = types::BlockNumber::new(99_000_000);
    assert_ne!(first.encode(), again.encode());
    assert_eq!(
        types::cctp::arrival_id(&first),
        types::cctp::arrival_id(&again)
    );
    let mut other_burn = first.clone();
    other_burn.nonce[0] ^= 1;
    assert_ne!(
        types::cctp::arrival_id(&first),
        types::cctp::arrival_id(&other_burn)
    );
    let mut other_domain = first.clone();
    other_domain.source_domain = 0;
    assert_ne!(
        types::cctp::arrival_id(&first),
        types::cctp::arrival_id(&other_domain)
    );

    on_fresh_memory(move || {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let source = TxHash::new([0x5c; 32]);
        let arrival = types::cctp::arrival_id(&first);
        append_event_at(created(finish_key(arrival, FinishAction::Complete), 0), at).unwrap();
        let id = append_event_at(signed(0, FinishAction::Complete, arrival, source), at)
            .unwrap()
            .get();
        // the re-attested message, asked for after the deadline, answers the completion
        assert_eq!(
            answered(FinishAction::Release, types::cctp::arrival_id(&again)),
            Ok(Some(id))
        );
    });
}

/// Review fix 4: what a finished arrival answers, whatever action a later call would
/// decide: its delivery, its release, or its completion, unless that reverted.
#[test]
fn a_finished_arrival_answers_any_later_call() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let source = TxHash::new([0x5c; 32]);
        let arrival = types::cctp::arrival_id(&ours(Action::Swap));
        assert_eq!(answered_arrival(arrival), Ok(None));
        let complete = finish_key(arrival, FinishAction::Complete);
        append_event_at(created(complete, 0), at).unwrap();
        assert!(answered_arrival(arrival).is_err(), "being signed");
        let id = append_event_at(signed(0, FinishAction::Complete, arrival, source), at)
            .unwrap()
            .get();
        assert_eq!(answered_arrival(arrival), Ok(Some(id)));
        append_event_at(
            EventType::Flow {
                quote_hash: complete,
                step: FlowStep::TxClosed {
                    tx_hash: TxHash::new([1; 32]),
                    block: types::BlockNumber::new(9),
                    success: false,
                },
            },
            at,
        )
        .unwrap();
        assert_eq!(answered_arrival(arrival), Ok(None), "a reverted completion");
        let release = finish_key(arrival, FinishAction::Release);
        append_event_at(created(release, 1), at).unwrap();
        let release_id = append_event_at(signed(1, FinishAction::Release, arrival, source), at)
            .unwrap()
            .get();
        assert_eq!(answered_arrival(arrival), Ok(Some(release_id)));
    });
}

/// `ours(action)` with its envelope edited, as any order's burn could seal it: the
/// canister reads every arrival, user-paid ones included.
fn sealed(action: Action, edit: impl Fn(&mut Envelope)) -> BurnMessage {
    let mut message = ours(action);
    let mut envelope = envelope(action);
    edit(&mut envelope);
    message.body.hook_data = envelope.encode();
    message
}

/// Review fix 6: a completion is signed only for an envelope the receiver and the Runner
/// will act on (abi.md rule 10), whoever paid the source: a recipient that is not zero and
/// none of Swapic's contracts, a Swap's tokenOut that is not zero or the arriving USDC
/// and a minOut above zero, and a minGas. A release pays the recipient USDC, so it needs a
/// recipient that is not zero.
#[test]
fn a_completion_needs_an_envelope_the_runner_acts_on() {
    let arbitrum = evm_chains().get(ARBITRUM).unwrap();
    let refused = |message: &BurnMessage| {
        assert!(
            matches!(
                decide_on(ARBITRUM, message, NOW),
                Err(FinishError::Arrival(_))
            ),
            "{:?}",
            decide_on(ARBITRUM, message, NOW)
        )
    };
    for action in [Action::Swap, Action::CkDeposit] {
        refused(&sealed(action, |e| e.recipient = EvmAddress::ZERO));
        for ours in [arbitrum.swapic, arbitrum.runner, arbitrum.cctp_receiver] {
            refused(&sealed(action, |e| e.recipient = ours));
        }
        refused(&sealed(action, |e| e.min_gas = 0));
    }
    refused(&sealed(Action::Swap, |e| e.token_out = EvmAddress::ZERO));
    refused(&sealed(Action::Swap, |e| e.token_out = arbitrum.usdc));
    refused(&sealed(Action::Swap, |e| e.min_out = U256::ZERO));
    // a CkDeposit names no tokenOut, and needs none
    assert!(decide_on(
        ARBITRUM,
        &sealed(Action::CkDeposit, |e| e.token_out = EvmAddress::ZERO),
        NOW
    )
    .is_ok());

    // after the deadline the release pays the recipient: an envelope fill would refuse is
    // still released, to a recipient that is not zero
    let late = DEADLINE + RELEASE_MARGIN_S + 1;
    let unfillable = sealed(Action::Swap, |e| e.min_out = U256::ZERO);
    assert_eq!(
        decide_on(ARBITRUM, &unfillable, late).map(|plan| plan.action),
        Ok(FinishAction::Release)
    );
    assert!(matches!(
        decide_on(
            ARBITRUM,
            &sealed(Action::Swap, |e| e.recipient = EvmAddress::ZERO),
            late
        ),
        Err(FinishError::Arrival(_))
    ));
}

/// Review fix 7: a release pays the envelope's EVM recipient the minted USDC and credits
/// no principal, so only that recipient is screened: a CkDeposit whose principal does not
/// read, or is listed, is still released, and a listed recipient is not.
#[test]
fn a_release_screens_only_the_recipient_it_pays() {
    on_fresh_memory(|| {
        crate::storage::init();
        let late = DEADLINE + RELEASE_MARGIN_S + 1;
        let unreadable = sealed(Action::CkDeposit, |e| e.principal = [0xff; 32]);
        let release = decide_on(ARBITRUM, &unreadable, late).unwrap();
        assert_eq!(release.action, FinishAction::Release);
        assert_eq!(screen(&release), Ok(()));
        // completing it is another matter: that credits the principal
        let complete = decide_on(ARBITRUM, &unreadable, NOW).unwrap();
        assert!(matches!(screen(&complete), Err(FinishError::Shape(_))));

        let principal = swapic::ck_principal(principal_word()).unwrap();
        sanctioned(principal.to_text());
        let listed = decide_on(ARBITRUM, &ours(Action::CkDeposit), late).unwrap();
        assert_eq!(
            screen(&listed),
            Ok(()),
            "the principal is not paid by a release"
        );
        sanctioned(USER.to_string());
        assert_eq!(
            screen(&listed),
            Err(FinishError::Sanctioned(Party::Evm(USER)))
        );
    });
}
