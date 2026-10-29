//! The consent messages on real orders (the staging canaries' own, `pool/pay/samples/`, and
//! the real KyberSwap route of `pool/plan/samples/scaler_base.json`), each rewritten only
//! where a payout plan differs from what its user signed (the canister its payer, and an Eco
//! reward's creator), every text pinned whole in Jeff's approved wording.

use super::*;
use alloy_primitives::{FixedBytes, U256};
use alloy_sol_types::SolCall;
use candid::{encode_args, encode_one, Nat};
use settlement_api::types::icrc21::ConsentMessageSpec;
use settlement_api::types::pool::{
    IcpAccount as WireAccount, PayoutPlan, PayoutTerms as WireTerms, PoolAccount as WirePool,
};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::swapic::{self, publishAndFundCall, Order, Payout, Step, KYBER_ROUTER};
use types::{CctpDomain, GasAmount, TokenAmount, Wei};

/// 28 Oct 2026, 12:15:00 UTC.
const DEADLINE_NS: u64 = 1_793_189_700_000_000_000;
/// 28 Oct 2026, 16:45:30 UTC.
const LATER_NS: u64 = 1_793_205_930_000_000_000;

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

fn canister() -> EvmAddress {
    address("0x36D34038F04990a58c068e08eD9754e2E3D9bA53")
}

fn base_usdc() -> EvmAddress {
    address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
}

fn arbitrum_usdc() -> EvmAddress {
    address("0xaf88d065e77c8cC2239327C5EDb3A432268e5831")
}

/// The staging deploy: one Swapic, Runner and CctpReceiver address on every chain.
fn chain(usdc: EvmAddress) -> EvmChain {
    EvmChain {
        swapic: address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"),
        runner: address("0x7E345acaB79102ac9588C60B9358Ec4d79438eEe"),
        cctp_receiver: address("0x23718C0a081B8bf00aee86722Af40E175562Ad47"),
        usdc,
        token_messenger: address("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"),
        message_transmitter: address("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"),
        gas_limit: PerAction::all(GasAmount::from(2_000_000_u64)),
        gas_budget: PerAction::all(Wei::from(10_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
    }
}

fn config() -> types::Config {
    types::Config {
        pool_chain: Some(ChainId::BASE),
        evm_chains: ChainTable(BTreeMap::from([
            (ChainId::BASE, chain(base_usdc())),
            (ChainId::ARBITRUM, chain(arbitrum_usdc())),
        ])),
        cctp_domains: ChainTable(BTreeMap::from([
            (ChainId::BASE, CctpDomain::new(6)),
            (ChainId::ARBITRUM, CctpDomain::new(3)),
        ])),
        eco_portal: Some(address("0xEC000769A73b70e16f361a442292500b3BCf4A85")),
        ..types::Config::default()
    }
}

fn ckusdc() -> Principal {
    Principal::from_text("xevnm-gaaaa-aaaar-qafnq-cai").unwrap()
}

fn icp() -> Principal {
    Principal::from_text("ryjl3-tyaaa-aaaaa-aaaba-cai").unwrap()
}

fn ckbtc() -> Principal {
    Principal::from_text("mxzaz-hqaaa-aaaar-qaada-cai").unwrap()
}

/// VCHF, on the routes tool's launch list but not the built-in list.
fn vchf() -> Principal {
    Principal::from_text("ly36x-wiaaa-aaaai-aqj7q-cai").unwrap()
}

/// kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe
fn staker() -> Principal {
    Principal::from_slice(&[0x52; 29])
}

/// im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc
fn receiver() -> Principal {
    Principal::from_slice(&[0x51; 29])
}

/// 5cawu-q2tkn-jvgu2-tknjv-gu2tk-njvgu-2tknj-vgu2t-knjvg-u2tkn-jvg
fn user() -> Principal {
    Principal::from_slice(&[0x53; 29])
}

const SEVENS: &str = "0707070707070707070707070707070707070707070707070707070707070707";

fn known(config: &types::Config, caller: Principal) -> Known<'_> {
    Known {
        config,
        canister: Some(canister()),
        ckusdc: Some(ckusdc()),
        caller,
    }
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

/// The Markdown a plain request answers (OISY's request: English, no offset, no device), or
/// the refusal.
fn generic(method: &str, arg: Vec<u8>, caller: Principal) -> Result<String, Icrc21Error> {
    let config = config();
    match answer(&request(method, arg), &known(&config, caller)) {
        ConsentMessageResponse::Ok(ConsentInfo {
            consent_message: ConsentMessage::GenericDisplayMessage(text),
            metadata,
        }) => {
            assert_eq!(
                metadata,
                ConsentMessageMetadata {
                    language: "en".to_string(),
                    utc_offset_minutes: Some(0)
                }
            );
            Ok(text)
        }
        ConsentMessageResponse::Ok(other) => panic!("not a generic message: {other:?}"),
        ConsentMessageResponse::Err(error) => Err(error),
    }
}

fn sample(hex_text: &str) -> Order {
    swapic::decode_order(&hex::decode(hex_text.trim()).unwrap()).unwrap()
}

/// A real order rewritten to be a payout plan: the canister its payer, and an Eco step's
/// reward creator.
fn as_plan(mut order: Order) -> Vec<u8> {
    order.payer = swapic::addr(canister());
    for step in &mut order.steps {
        if swapic::selector_of(step) == Some(swapic::PUBLISH_AND_FUND) {
            let mut call = publishAndFundCall::abi_decode(&step.data).unwrap();
            call.reward.creator = swapic::addr(canister());
            step.data = call.abi_encode().into();
        }
    }
    swapic::encode_order(&order)
}

/// 5 USDC on Base to USDC on Arbitrum through Eco: at least 5 USDC to 0xf00E...89AC.
fn eco_plan() -> Vec<u8> {
    as_plan(sample(include_str!("../pool/pay/samples/eco_base_to_arbitrum.hex")))
}

/// 1 USDC to ether on Base through KyberSwap: at least 408660260803591 wei to 0xf00E...89AC.
fn ether_plan() -> Vec<u8> {
    as_plan(sample(include_str!("../pool/pay/samples/same_chain_base.hex")))
}

/// 500 USDC to WETH on Base through the real KyberSwap route, at least 0.2 WETH to
/// 0x2222...2222.
fn weth_plan() -> Vec<u8> {
    let sample: serde_json::Value =
        serde_json::from_str(include_str!("../pool/plan/samples/scaler_base.json")).unwrap();
    let route = hex::decode(
        sample["route_calldata"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
    )
    .unwrap();
    swapic::encode_order(&Order {
        r#ref: FixedBytes([0x62; 32]),
        payer: swapic::addr(canister()),
        tokenIn: swapic::addr(base_usdc()),
        amountIn: U256::from(500_000_000_u64),
        deadline: U256::from(1_793_189_700_u64),
        steps: vec![Step {
            target: swapic::addr(KYBER_ROUTER),
            data: route.into(),
            token: swapic::addr(base_usdc()),
            min: U256::ZERO,
            writes: vec![],
        }],
        payout: Payout {
            token: swapic::addr(address("0x4200000000000000000000000000000000000006")),
            recipient: swapic::addr(address("0x2222222222222222222222222222222222222222")),
            minOut: U256::from(200_000_000_000_000_000_u64),
        },
    })
}

/// Synthetic, for the raw form only: no step, paying 12345 units of a token no list names on
/// Base to 0x2222...2222.
fn unlisted_plan() -> Vec<u8> {
    swapic::encode_order(&Order {
        r#ref: FixedBytes([0x63; 32]),
        payer: swapic::addr(canister()),
        tokenIn: swapic::addr(base_usdc()),
        amountIn: U256::from(5_000_000_u64),
        deadline: U256::from(1_793_189_700_u64),
        steps: vec![],
        payout: Payout {
            token: swapic::addr(address("0x3333333333333333333333333333333333333333")),
            recipient: swapic::addr(address("0x2222222222222222222222222222222222222222")),
            minOut: U256::from(12_345_u64),
        },
    })
}

fn swap_arg(
    token_in: Principal,
    amount_in: u64,
    from_subaccount: Option<Vec<u8>>,
    refund_to: Option<WireAccount>,
    deadline_ns: u64,
    order: Vec<u8>,
) -> Vec<u8> {
    encode_one(IcpSwapTerms {
        token_in,
        amount_in: Nat::from(amount_in),
        from_subaccount,
        refund_to,
        slippage_bps: 100,
        deadline_ns,
        plan: PayoutPlan {
            order,
            gas_limit: 600_000,
        },
    })
    .unwrap()
}

fn stake_arg(owner: WirePool, from_subaccount: Option<Vec<u8>>) -> Vec<u8> {
    encode_one(StakeIcpArgs {
        from_subaccount,
        amount: Nat::from(250_000_000_u64),
        owner,
    })
    .unwrap()
}

fn icp_account(owner: Principal, subaccount: Option<Vec<u8>>) -> WirePool {
    WirePool::Icp(WireAccount { owner, subaccount })
}

fn withdrawal_arg() -> Vec<u8> {
    encode_args((
        EvmWithdrawal {
            staker: "0x1111111111111111111111111111111111111111".to_string(),
            usdc: Nat::from(10_000_000_u64),
            max_shares: Nat::from(10_000_000_000_000_000_000_000_u128),
            rebalance_share: Nat::from(2_000_u64),
            network_fee: Nat::from(50_000_u64),
            payout: WireTerms {
                dst_chain: 42161,
                recipient: "0x2222222222222222222222222222222222222222".to_string(),
                token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(),
                min_out: Nat::from(9_900_000_u64),
                deadline_ns: DEADLINE_NS,
            },
            nonce: 7,
        },
        vec![0x1b_u8; 65],
    ))
    .unwrap()
}

/// Jeff's approved shape, word for word.
#[test]
fn a_swap_to_arbitrum_states_what_leaves_what_arrives_and_the_refund() {
    let text = generic(
        "swap_from_icp",
        swap_arg(ckusdc(), 5_100_000, None, None, DEADLINE_NS, eco_plan()),
        user(),
    );
    assert_eq!(
        text.unwrap(),
        "## Swap ckUSDC to Arbitrum\n\n\
         - You send: 5.1 ckUSDC, plus the ledger fee\n\
         - From: your account\n\
         - Recipient gets at least: 5 USDC on Arbitrum\n\
         - Token: USDC (0xaf88d065e77c8cC2239327C5EDb3A432268e5831)\n\
         - Recipient: 0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC\n\
         - Deadline: 28 Oct 2026, 12:15 UTC\n\
         - If it can't finish: refunded to your account, in ckUSDC or the token you sent, less \
         ledger fees"
    );
}

/// Jeff's approved shape, with a subaccount and another refund account shown in full.
#[test]
fn a_swap_of_icp_to_ether_names_base_s_own_coin_and_another_refund_account() {
    let text = generic(
        "swap_from_icp",
        swap_arg(
            icp(),
            25_000_000,
            Some(vec![7; 32]),
            Some(WireAccount { owner: receiver(), subaccount: None }),
            LATER_NS,
            ether_plan(),
        ),
        user(),
    );
    assert_eq!(
        text.unwrap(),
        format!(
            "## Swap ICP to Base\n\n\
             - You send: 0.25 ICP, plus the ledger fee\n\
             - From: your account (subaccount {SEVENS})\n\
             - Recipient gets at least: 0.000408660260803591 ETH on Base\n\
             - Token: ETH (Base's own coin)\n\
             - Recipient: 0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC\n\
             - Deadline: 28 Oct 2026, 16:45:30 UTC\n\
             - If it can't finish: refunded to \
             im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc, in ckUSDC or the \
             token you sent, less ledger fees"
        )
    );
}

/// Both tokens from the built-in list, asked anonymously (a cold signer).
#[test]
fn a_swap_of_ckbtc_to_weth_names_both_from_the_built_in_list() {
    let text = generic(
        "swap_from_icp",
        swap_arg(ckbtc(), 250_000, None, None, DEADLINE_NS, weth_plan()),
        Principal::anonymous(),
    );
    assert_eq!(
        text.unwrap(),
        "## Swap ckBTC to Base\n\n\
         - You send: 0.0025 ckBTC, plus the ledger fee\n\
         - From: your account\n\
         - Recipient gets at least: 0.2 WETH on Base\n\
         - Token: WETH (0x4200000000000000000000000000000000000006)\n\
         - Recipient: 0x2222222222222222222222222222222222222222\n\
         - Deadline: 28 Oct 2026, 12:15 UTC\n\
         - If it can't finish: refunded to your account, in ckUSDC or the token you sent, less \
         ledger fees"
    );
}

/// Review Focus 3, asked anonymously.
#[test]
fn a_swap_of_an_unlisted_token_to_an_unlisted_token_names_both_by_id() {
    let text = generic(
        "swap_from_icp",
        swap_arg(vchf(), 250_000, None, None, DEADLINE_NS, unlisted_plan()),
        Principal::anonymous(),
    );
    assert_eq!(
        text.unwrap(),
        "## Swap an ICP token to Base\n\n\
         - You send: 250000 units of token ly36x-wiaaa-aaaai-aqj7q-cai, plus the ledger fee\n\
         - From: your account\n\
         - Recipient gets at least: 12345 units of token \
         0x3333333333333333333333333333333333333333 on Base\n\
         - Token: 0x3333333333333333333333333333333333333333\n\
         - Recipient: 0x2222222222222222222222222222222222222222\n\
         - Deadline: 28 Oct 2026, 12:15 UTC\n\
         - If it can't finish: refunded to your account, in ckUSDC or the token you sent, less \
         ledger fees"
    );
}

/// Review Focus 1: Jeff's approved shape, then another owner shown in full.
#[test]
fn a_stake_is_described_to_its_staker_and_to_a_cold_signer() {
    let header = "## Stake ckUSDC in the Swapic pool\n\n\
                  - You stake: 250 ckUSDC, plus the ledger fee\n\
                  - From: your account\n";
    let own = stake_arg(icp_account(staker(), None), None);
    assert_eq!(
        generic("stake_icp", own.clone(), staker()).unwrap(),
        format!("{header}- Stake owned by: your account")
    );
    assert_eq!(
        generic("stake_icp", own, Principal::anonymous()).unwrap(),
        format!(
            "{header}- Stake owned by: \
             kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe"
        )
    );
    assert_eq!(
        generic("stake_icp", stake_arg(icp_account(staker(), Some(vec![7; 32])), None), staker())
            .unwrap(),
        format!(
            "{header}- Stake owned by: \
             kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe (this wallet, \
             subaccount {SEVENS})"
        )
    );
    assert_eq!(
        generic(
            "stake_icp",
            stake_arg(WirePool::Evm("0x1111111111111111111111111111111111111111".to_string()), None),
            staker()
        )
        .unwrap(),
        format!("{header}- Stake owned by: the EVM address 0x1111111111111111111111111111111111111111")
    );
}

/// Jeff's approved shape (both payouts), with a stake subaccount and another account in full.
#[test]
fn a_withdrawal_on_icp_states_who_is_paid_at_least_what() {
    let wire = |from: Option<Vec<u8>>, token_out: Principal, min_out: u64, to: WireAccount| {
        encode_one(WithdrawIcpArgs {
            from,
            usdc: Nat::from(100_000_000_u64),
            max_shares: Nat::from(100_000_000_000_000_000_000_000_u128),
            token_out,
            min_out: Nat::from(min_out),
            to,
        })
        .unwrap()
    };
    let to_self = wire(None, ckusdc(), 99_990_000, WireAccount { owner: staker(), subaccount: None });
    assert_eq!(
        generic("withdraw_icp", to_self, staker()).unwrap(),
        "## Withdraw from the Swapic pool on ICP\n\n\
         - You withdraw: 100 USDC from your stake\n\
         - Paid to: your account\n\
         - Paid at least: 99.99 ckUSDC\n\
         - Fees: ledger fees come out of the 100"
    );
    let as_icp = wire(
        Some(vec![7; 32]),
        icp(),
        1_250_000_000,
        WireAccount { owner: receiver(), subaccount: None },
    );
    assert_eq!(
        generic("withdraw_icp", as_icp, staker()).unwrap(),
        format!(
            "## Withdraw from the Swapic pool on ICP\n\n\
             - You withdraw: 100 USDC from your stake (subaccount {SEVENS})\n\
             - Paid to: im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc\n\
             - Paid at least: 12.5 ICP\n\
             - Fees: ledger fees come out of the 100\n\
             - If the swap pays less: all of it is paid as ckUSDC"
        )
    );
}

/// Jeff's approved shape, with the Token, Recipient and Deadline lines the swap has.
#[test]
fn a_withdrawal_to_evm_states_the_staker_what_is_kept_and_what_arrives() {
    assert_eq!(
        generic("withdraw_evm", withdrawal_arg(), Principal::anonymous()).unwrap(),
        "## Withdraw from the Swapic pool to Arbitrum\n\n\
         - Staker: 0x1111111111111111111111111111111111111111 (their signature allows this)\n\
         - Withdraw: 10 USDC from the stake\n\
         - Kept: 0.002 USDC pool fee, 0.05 USDC network fee\n\
         - Recipient gets at least: 9.9 USDC on Arbitrum\n\
         - Token: USDC (0xaf88d065e77c8cC2239327C5EDb3A432268e5831)\n\
         - Recipient: 0x2222222222222222222222222222222222222222\n\
         - Deadline: 28 Oct 2026, 12:15 UTC\n\
         - If it can't finish: the withdrawal is undone and the stake is restored"
    );
}

/// Review Focus 4.
#[test]
fn a_fields_display_gets_the_same_facts_as_plain_fields() {
    let config = config();
    let mut asked = request("withdraw_evm", withdrawal_arg());
    asked.user_preferences.device_spec = Some(DeviceSpec::FieldsDisplay);
    let text = |content: &str| Value::Text(TextValue { content: content.to_string() });
    assert_eq!(
        answer(&asked, &known(&config, Principal::anonymous())),
        ConsentMessageResponse::Ok(ConsentInfo {
            consent_message: ConsentMessage::FieldsDisplayMessage(FieldsDisplay {
                intent: "Withdraw from the Swapic pool to Arbitrum".to_string(),
                fields: vec![
                    (
                        "Staker".to_string(),
                        text("0x1111111111111111111111111111111111111111 (their signature allows this)")
                    ),
                    ("Withdraw".to_string(), text("10 USDC from the stake")),
                    ("Kept".to_string(), text("0.002 USDC pool fee, 0.05 USDC network fee")),
                    ("Recipient gets at least".to_string(), text("9.9 USDC on Arbitrum")),
                    (
                        "Token".to_string(),
                        text("USDC (0xaf88d065e77c8cC2239327C5EDb3A432268e5831)")
                    ),
                    ("Recipient".to_string(), text("0x2222222222222222222222222222222222222222")),
                    (
                        "Deadline".to_string(),
                        Value::TimestampSeconds(TimestampSeconds { amount: 1_793_189_700 })
                    ),
                    (
                        "If it can't finish".to_string(),
                        text("the withdrawal is undone and the stake is restored")
                    ),
                ],
            }),
            metadata: ConsentMessageMetadata {
                language: "en".to_string(),
                utc_offset_minutes: None
            },
        })
    );
}

/// Review Focus 4.
#[test]
fn the_deadline_is_written_in_the_offset_asked_and_the_metadata_says_which() {
    let config = config();
    let ask = |offset: Option<i16>, language: &str| {
        let mut asked = request(
            "swap_from_icp",
            swap_arg(ckusdc(), 5_100_000, None, None, DEADLINE_NS, eco_plan()),
        );
        asked.user_preferences.metadata = ConsentMessageMetadata {
            language: language.to_string(),
            utc_offset_minutes: offset,
        };
        match answer(&asked, &known(&config, user())) {
            ConsentMessageResponse::Ok(ConsentInfo {
                consent_message: ConsentMessage::GenericDisplayMessage(text),
                metadata,
            }) => (text, metadata),
            other => panic!("{other:?}"),
        }
    };
    let (text, metadata) = ask(Some(240), "en");
    assert!(text.contains("\n- Deadline: 28 Oct 2026, 16:15 UTC+04:00\n"), "{text}");
    assert_eq!(metadata.utc_offset_minutes, Some(240));
    let (text, metadata) = ask(Some(10_000), "en");
    assert!(text.contains("\n- Deadline: 28 Oct 2026, 12:15 UTC\n"), "{text}");
    assert_eq!(metadata.utc_offset_minutes, Some(0));
    let (german, metadata) = ask(None, "de-CH");
    assert_eq!(metadata.language, "en");
    assert_eq!(german, ask(None, "en").0);
}

#[test]
fn both_displays_state_the_same_lines_in_the_same_order() {
    let config = config();
    let calls = [
        ("stake_icp", stake_arg(icp_account(staker(), None), None)),
        ("withdraw_evm", withdrawal_arg()),
        ("swap_from_icp", swap_arg(ckusdc(), 5_100_000, None, None, DEADLINE_NS, eco_plan())),
    ];
    for (method, arg) in calls {
        let consent = consent(method, &arg, &known(&config, staker())).unwrap();
        let markdown = consent.markdown(UtcOffset::of(None));
        let fields = consent.fields();
        assert_eq!(markdown.lines().count(), fields.fields.len() + 2, "{method}");
        for ((label, _), line) in fields.fields.iter().zip(markdown.lines().skip(2)) {
            assert!(line.starts_with(&format!("- {label}: ")), "{method}: {line}");
        }
    }
}

/// No message carries the words Jeff ruled out.
#[test]
fn no_message_says_rebalancing_position_or_a_chain_id() {
    let config = config();
    let calls = [
        ("stake_icp", stake_arg(icp_account(staker(), None), None)),
        ("withdraw_evm", withdrawal_arg()),
        ("swap_from_icp", swap_arg(ckusdc(), 5_100_000, None, None, DEADLINE_NS, eco_plan())),
        ("swap_from_icp", swap_arg(icp(), 25_000_000, None, None, LATER_NS, ether_plan())),
    ];
    for (method, arg) in calls {
        let text = consent(method, &arg, &known(&config, staker()))
            .unwrap()
            .markdown(UtcOffset::of(None));
        for banned in ["rebalancing", "position", "(chain "] {
            assert!(!text.contains(banned), "{method}: {banned} in {text}");
        }
    }
}

fn unsupported(answer: ConsentMessageResponse) -> String {
    match answer {
        ConsentMessageResponse::Err(Icrc21Error::UnsupportedCanisterCall(ErrorInfo {
            description,
        })) => description,
        other => panic!("not refused as unsupported: {other:?}"),
    }
}

fn not_now(answer: ConsentMessageResponse) -> String {
    match answer {
        ConsentMessageResponse::Err(Icrc21Error::ConsentMessageUnavailable(ErrorInfo {
            description,
        })) => description,
        other => panic!("not refused as unavailable: {other:?}"),
    }
}

#[test]
fn any_other_method_is_refused_by_name_bounded() {
    let config = config();
    let ask = |method: &str| answer(&request(method, vec![]), &known(&config, staker()));
    assert_eq!(
        unsupported(ask("set_config")),
        "set_config has no consent message: Swapic describes only swap_from_icp, stake_icp, \
         withdraw_icp and withdraw_evm"
    );
    let description = unsupported(ask(&"a".repeat(100_000)));
    assert!(description.len() <= MAX_DESCRIPTION_BYTES + 3, "{}", description.len());
}

/// Review Focus 2 and 5.
#[test]
fn an_argument_the_door_would_not_take_is_refused_never_trapped() {
    let config = config();
    let k = known(&config, staker());
    let ask = |method: &str, arg: Vec<u8>| answer(&request(method, arg), &k);
    assert!(unsupported(ask("stake_icp", encode_one("not a stake").unwrap()))
        .starts_with("the argument of stake_icp does not decode: "));
    assert!(unsupported(ask("stake_icp", vec![0xff; 8]))
        .starts_with("the argument of stake_icp does not decode: "));
    assert_eq!(
        unsupported(ask("stake_icp", stake_arg(icp_account(staker(), None), Some(vec![1; 31])))),
        "stake_icp: NotAllowed(\"from_subaccount must be 32 bytes, not 31\")"
    );
    let huge = encode_one(IcpSwapTerms {
        token_in: ckusdc(),
        amount_in: Nat::from(u128::MAX) + Nat::from(1_u8),
        from_subaccount: None,
        refund_to: None,
        slippage_bps: 100,
        deadline_ns: DEADLINE_NS,
        plan: PayoutPlan { order: eco_plan(), gas_limit: 600_000 },
    })
    .unwrap();
    assert_eq!(
        unsupported(ask("swap_from_icp", huge)),
        "swap_from_icp: NotAllowed(\"amount_in does not fit 128 bits\")"
    );
    assert_eq!(
        unsupported(ask(
            "swap_from_icp",
            swap_arg(ckusdc(), 1, None, None, DEADLINE_NS, vec![0xab; 10])
        )),
        "swap_from_icp: NotAllowed(\"the payout plan is not abi.encode of an Order\")"
    );
    assert_eq!(
        unsupported(ask(
            "swap_from_icp",
            swap_arg(ckusdc(), 1, None, None, DEADLINE_NS, vec![0; 16 * 1_024 + 1])
        )),
        "swap_from_icp: NotAllowed(\"a payout plan is at most 16384 bytes\")"
    );
    let mut unreadable = sample(include_str!("../pool/pay/samples/same_chain_base.hex"));
    unreadable.steps[0].data = vec![0xde, 0xad, 0xbe, 0xef].into();
    assert!(unsupported(ask(
        "swap_from_icp",
        swap_arg(ckusdc(), 1, None, None, DEADLINE_NS, as_plan(unreadable))
    ))
    .starts_with("swap_from_icp: NotAllowed(\"the plan step 0 is none of the shapes"));
    let (mut withdrawal, signature): (EvmWithdrawal, Vec<u8>) =
        candid::decode_args(&withdrawal_arg()).unwrap();
    withdrawal.staker = "0xnotanaddress".to_string();
    assert_eq!(
        unsupported(ask("withdraw_evm", encode_args((withdrawal, signature)).unwrap())),
        "withdraw_evm: NotAllowed(\"staker is not an EVM address\")"
    );
}

/// Review Focus 5: what the canister has not set yet makes a swap's plan unreadable now.
#[test]
fn a_swap_the_canister_cannot_read_yet_is_unavailable() {
    let config = config();
    let arg = swap_arg(ckusdc(), 5_100_000, None, None, DEADLINE_NS, eco_plan());
    let no_address = Known { canister: None, ..known(&config, user()) };
    assert_eq!(
        not_now(answer(&request("swap_from_icp", arg.clone()), &no_address)),
        "swap_from_icp: Unavailable(\"the canister's address is not derived yet\")"
    );
    let no_pool_chain = types::Config { pool_chain: None, ..config.clone() };
    assert_eq!(
        not_now(answer(&request("swap_from_icp", arg), &known(&no_pool_chain, user()))),
        "swap_from_icp: Closed(\"no pool chain is configured\")"
    );
}

#[test]
fn the_standards_are_icrc10_and_icrc21() {
    assert_eq!(
        supported_standards(),
        vec![
            SupportedStandard {
                name: "ICRC-10".to_string(),
                url: "https://github.com/dfinity/ICRC/blob/main/ICRCs/ICRC-10/ICRC-10.md"
                    .to_string(),
            },
            SupportedStandard {
                name: "ICRC-21".to_string(),
                url: "https://github.com/dfinity/ICRC/blob/main/ICRCs/ICRC-21/ICRC-21.md"
                    .to_string(),
            },
        ]
    );
}
