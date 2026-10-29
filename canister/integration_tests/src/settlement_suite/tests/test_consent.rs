//! ICRC-21 and ICRC-10 on the installed canister: the wallet's two calls answer, the paying
//! wallet and a cold signer get the same facts, nothing is written, and the call is cheap.

use crate::client::settlement::{
    event_count, icrc10_supported_standards, icrc21_canister_call_consent_message,
};
use crate::settlement_suite::init::setup;
use candid::{encode_one, Nat, Principal};
use settlement_api::types::icrc21::{
    ConsentInfo, ConsentMessage, ConsentMessageMetadata, ConsentMessageRequest,
    ConsentMessageResponse, ConsentMessageSpec, ErrorInfo, Icrc21Error,
};
use settlement_api::types::pool::{
    IcpAccount, IcpSwapTerms, PayoutPlan, PoolAccount, StakeIcpArgs,
};

/// kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe
fn staker() -> Principal {
    Principal::from_slice(&[0x52; 29])
}

fn request(method: &str, arg: Vec<u8>) -> ConsentMessageRequest {
    ConsentMessageRequest {
        method: method.to_string(),
        arg,
        user_preferences: ConsentMessageSpec {
            metadata: ConsentMessageMetadata {
                language: "en".to_string(),
                utc_offset_minutes: None,
            },
            device_spec: None,
        },
    }
}

fn stake_arg() -> Vec<u8> {
    encode_one(StakeIcpArgs {
        from_subaccount: None,
        amount: Nat::from(250_000_000_u64),
        owner: PoolAccount::Icp(IcpAccount {
            owner: staker(),
            subaccount: None,
        }),
    })
    .unwrap()
}

fn generic(text: &str) -> ConsentMessageResponse {
    ConsentMessageResponse::Ok(ConsentInfo {
        consent_message: ConsentMessage::GenericDisplayMessage(text.to_string()),
        metadata: ConsentMessageMetadata {
            language: "en".to_string(),
            utc_offset_minutes: Some(0),
        },
    })
}

/// Jeff's approved shape (2026-10-28).
const STAKE_HEADER: &str = "## Stake ckUSDC in the Swapic pool\n\n\
    - You stake: 250 ckUSDC, plus the ledger fee\n\
    - From: your account\n";

#[test]
fn icrc10_lists_icrc10_and_icrc21() {
    let (pic, canister, _) = setup();
    let names: Vec<String> = icrc10_supported_standards(&pic, canister, Principal::anonymous())
        .into_iter()
        .map(|standard| standard.name)
        .collect();
    assert_eq!(names, ["ICRC-10", "ICRC-21"]);
}

#[test]
fn a_stake_is_described_to_its_staker_and_to_a_cold_signer_and_nothing_is_written() {
    let (pic, canister, admin) = setup();
    for _ in 0..5 {
        pic.tick();
    }
    let before = event_count(&pic, canister, admin);
    let cycles = pic.cycle_balance(canister);
    let mine = icrc21_canister_call_consent_message(
        &pic,
        canister,
        staker(),
        &request("stake_icp", stake_arg()),
    );
    let spent = cycles - pic.cycle_balance(canister);
    eprintln!("CONSENT cycles for one stake description: {spent}");
    assert!(spent < 50_000_000, "{spent}");
    assert_eq!(
        mine,
        generic(&format!("{STAKE_HEADER}- Stake owned by: your account"))
    );
    let cold = icrc21_canister_call_consent_message(
        &pic,
        canister,
        Principal::anonymous(),
        &request("stake_icp", stake_arg()),
    );
    assert_eq!(
        cold,
        generic(&format!(
            "{STAKE_HEADER}- Stake owned by: \
             kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe"
        ))
    );
    assert_eq!(event_count(&pic, canister, admin), before, "nothing written");
}

#[test]
fn another_method_is_unsupported_and_a_swap_before_the_address_is_unavailable() {
    use alloy_primitives::{FixedBytes, U256};
    use types::swapic::{addr, encode_order, Order, Payout};
    let (pic, canister, _) = setup();
    let other = icrc21_canister_call_consent_message(
        &pic,
        canister,
        staker(),
        &request("set_config", vec![]),
    );
    assert_eq!(
        other,
        ConsentMessageResponse::Err(Icrc21Error::UnsupportedCanisterCall(ErrorInfo {
            description: "set_config has no consent message: Swapic describes only \
                          swap_from_icp, stake_icp, withdraw_icp and withdraw_evm"
                .to_string()
        }))
    );
    let usdc: types::EvmAddress = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();
    let to: types::EvmAddress = "0x2222222222222222222222222222222222222222".parse().unwrap();
    let plan = encode_order(&Order {
        r#ref: FixedBytes([0x61; 32]),
        payer: addr(types::EvmAddress::new([0x36; 20])),
        tokenIn: addr(usdc),
        amountIn: U256::from(5_000_000_u64),
        deadline: U256::from(1_793_189_700_u64),
        steps: vec![],
        payout: Payout {
            token: addr(usdc),
            recipient: addr(to),
            minOut: U256::from(4_900_000_u64),
        },
    });
    let swap = encode_one(IcpSwapTerms {
        token_in: Principal::from_text("xevnm-gaaaa-aaaar-qafnq-cai").unwrap(),
        amount_in: Nat::from(5_100_000_u64),
        from_subaccount: None,
        refund_to: None,
        slippage_bps: 100,
        deadline_ns: 1_793_189_700_000_000_000,
        plan: PayoutPlan {
            order: plan,
            gas_limit: 300_000,
        },
    })
    .unwrap();
    let answer = icrc21_canister_call_consent_message(
        &pic,
        canister,
        staker(),
        &request("swap_from_icp", swap),
    );
    assert_eq!(
        answer,
        ConsentMessageResponse::Err(Icrc21Error::ConsentMessageUnavailable(ErrorInfo {
            description: "swap_from_icp: Unavailable(\"the canister's address is not derived yet\")"
                .to_string()
        }))
    );
}
