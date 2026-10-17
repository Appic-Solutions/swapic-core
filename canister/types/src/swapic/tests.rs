use super::*;
use alloy_primitives::{address, b256, bytes};
use alloy_sol_types::SolEvent;

/// abi.md section 2's exact client message.
fn golden() -> Order {
    let token_in = address!("3333333333333333333333333333333333333333");
    Order {
        r#ref: FixedBytes([0x11; 32]),
        payer: address!("2222222222222222222222222222222222222222"),
        tokenIn: token_in,
        amountIn: U256::from(1_000_000_000_000_000_000u128),
        deadline: U256::from(1_790_000_000u64),
        steps: vec![
            Step {
                target: address!("4444444444444444444444444444444444444444"),
                data: bytes!("deadbeef01"),
                token: token_in,
                min: U256::from(1_000_000_000_000_000_000u128),
                writes: vec![],
            },
            Step {
                target: address!("5555555555555555555555555555555555555555"),
                data: bytes!("cafebabe00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001"),
                token: address!("6666666666666666666666666666666666666666"),
                min: U256::from(2_500_000_000u64),
                writes: vec![
                    Write { at: 4, minus: U256::ZERO },
                    Write { at: 37, minus: U256::from(57_115u64) },
                ],
            },
        ],
        payout: Payout {
            token: address!("7777777777777777777777777777777777777777"),
            recipient: address!("8888888888888888888888888888888888888888"),
            minOut: U256::from(999u64),
        },
    }
}

const GOLDEN_SWAPIC: EvmAddress = EvmAddress::new([0xbb; 20]);
const BASE: ChainId = ChainId::BASE;

#[test]
fn order_id_matches_abi_golden() {
    assert_eq!(
        domain(BASE, GOLDEN_SWAPIC).separator(),
        b256!("c395d776d27009143a498a5ebdf6ab4be54d50c85087e1f2c598ceb460bb8b51")
    );
    assert_eq!(
        order_id(&golden(), BASE, GOLDEN_SWAPIC).into_bytes(),
        b256!("2f21eee628b331d82ca45f067ec51c73de0533289d22c22328cf4ebd43e255bc").0
    );
    assert_eq!(
        acceptance_digest(&golden(), BASE, GOLDEN_SWAPIC),
        b256!("5058c6a5e030b5beb7c0a929627f189d69cbf98e8801d7e5bb868f4805937d0d").0
    );
    // any change anywhere is another id
    let mut changed = golden();
    changed.steps[1].writes[1].minus = U256::from(57_116u64);
    assert_ne!(
        order_id(&changed, BASE, GOLDEN_SWAPIC),
        order_id(&golden(), BASE, GOLDEN_SWAPIC)
    );
    assert_ne!(
        order_id(&golden(), ChainId::ARBITRUM, GOLDEN_SWAPIC),
        order_id(&golden(), BASE, GOLDEN_SWAPIC)
    );
}

#[test]
fn typehashes_match_abi_table() {
    let typehash = |t: &str| keccak256(t).0;
    assert_eq!(
        typehash(ORDER_TYPE),
        b256!("d46b04ddb0d039fd8d858827e8603da1a6d43b2e1c788328fd321b4782f2cb7d").0
    );
    assert_eq!(
        typehash(STEP_TYPE),
        b256!("0682eb4409dbcbd035b45046e7629810578851f37499e32990aefe0c8dbc2134").0
    );
    assert_eq!(
        typehash(WRITE_TYPE),
        b256!("c4688b5dd66453b7f007d0ec6009b512f5df252efe9072aff82a09a6779744c3").0
    );
    assert_eq!(
        typehash(PAYOUT_TYPE),
        b256!("73644d236b331510ff1d9a455c49a20d6dd646f5a4c449a25ee91053263f8369").0
    );
    assert_eq!(
        typehash(ACCEPTANCE_TYPE),
        b256!("0542f436365880f872dcd8b65071eea3eecb5e112bbdf0f91b3cd9369022c9eb").0
    );
    assert_eq!(
        typehash(WITNESS_TYPE),
        b256!("19f3012ade9f169fdc7a2a111fc8418a642d6abe974245ea52dbb4a626b7cbc4").0
    );
}

#[test]
fn selectors_and_topics_match_abi_section_8() {
    let sel = |s: [u8; 4]| hex::encode(s);
    assert_eq!(sel(selectors::RUN_FOR), "f7ede948");
    assert_eq!(sel(selectors::COMPLETE_CCTP), "513429dd");
    assert_eq!(sel(selectors::RELEASE), "aa30d53a");
    assert_eq!(sel(selectors::RECEIVE_MESSAGE), "57ecfd28");
    assert_eq!(sel(selectors::TRANSFER), "a9059cbb");
    assert_eq!(sel(selectors::DEPOSIT_FOR_BURN_WITH_HOOK), "779b432d");
    assert_eq!(
        OrderStarted::SIGNATURE_HASH,
        b256!("8e97ed8d4d8c9c453242f7e97e8fceb3b04813c3257d5d5a11d74b4cf89b5e82")
    );
}

fn envelope_golden() -> Envelope {
    Envelope {
        reference: [0x11; 32],
        action: Action::Swap,
        deadline: 1_790_003_600,
        recipient: EvmAddress::new([0x66; 20]),
        token_out: EvmAddress::new([0x77; 20]),
        min_out: U256::from(999u64),
        principal: [0x88; 32],
        subaccount: [0x99; 32],
        min_gas: 700_000,
    }
}

#[test]
fn the_envelope_is_the_abi_golden() {
    let words = "0000000000000000000000000000000000000000000000000000000000000003\
        1111111111111111111111111111111111111111111111111111111111111111\
        0000000000000000000000000000000000000000000000000000000000000001\
        000000000000000000000000000000000000000000000000000000006ab14990\
        0000000000000000000000006666666666666666666666666666666666666666\
        0000000000000000000000007777777777777777777777777777777777777777\
        00000000000000000000000000000000000000000000000000000000000003e7\
        8888888888888888888888888888888888888888888888888888888888888888\
        9999999999999999999999999999999999999999999999999999999999999999\
        00000000000000000000000000000000000000000000000000000000000aae60";
    let encoded = envelope_golden().encode();
    assert_eq!(encoded.len(), Envelope::LENGTH);
    assert_eq!(hex::encode(&encoded), words);
    assert_eq!(Envelope::decode(&encoded), Ok(envelope_golden()));
    // in a 696-byte CCTP message, at 376
    let mut message = vec![0u8; Envelope::CCTP_OFFSET];
    message.extend_from_slice(&encoded);
    assert_eq!(message.len(), Envelope::CCTP_MESSAGE_LENGTH);
    assert_eq!(Envelope::of_cctp_message(&message), Ok(envelope_golden()));
}

#[test]
fn an_envelope_the_contracts_refuse_does_not_decode() {
    let good = envelope_golden().encode();
    assert_eq!(
        Envelope::decode(&good[..319]),
        Err(EnvelopeError::Length(319))
    );
    let mut v2 = good.clone();
    v2[31] = 2;
    assert!(matches!(
        Envelope::decode(&v2),
        Err(EnvelopeError::Version(_))
    ));
    let mut action = good.clone();
    action[95] = 3;
    assert!(matches!(
        Envelope::decode(&action),
        Err(EnvelopeError::Action(_))
    ));
    // an address word with high bits
    let mut dirty = good.clone();
    dirty[4 * 32] = 1;
    assert_eq!(Envelope::decode(&dirty), Err(EnvelopeError::Range(4)));
    // a deadline above uint64
    let mut late = good;
    late[3 * 32 + 23] = 1;
    assert_eq!(Envelope::decode(&late), Err(EnvelopeError::Range(3)));
}

#[test]
fn the_order_round_trips_and_run_for_carries_it() {
    let order = golden();
    let encoded = encode_order(&order);
    // abi.encode of one dynamic tuple starts with its offset word
    assert_eq!(
        &encoded[..32],
        U256::from(0x20).to_be_bytes::<32>().as_slice()
    );
    assert_eq!(decode_order(&encoded), Some(order.clone()));
    let auth = GaslessAuth {
        door: DOOR_AUTH_3009,
        validBefore: U256::from(1_790_000_000u64),
        nonce: order_id(&order, BASE, GOLDEN_SWAPIC).as_nonce(),
        ..Default::default()
    };
    assert_eq!(decode_auth(&encode_auth(&auth)), Some(auth.clone()));
    let data = run_for_calldata(&order, &auth);
    assert_eq!(&data[..4], selectors::RUN_FOR);
    let call = runForCall::abi_decode(&data).unwrap();
    assert_eq!((call.order, call.auth), (order, auth));
}

#[test]
fn a_fee_step_is_read_back_and_nothing_else_is_one() {
    let usdc = EvmAddress::new([0x83; 20]);
    let canister = EvmAddress::new([0xca; 20]);
    let fee = Step {
        target: addr(usdc),
        data: transferCall {
            to: addr(canister),
            amount: U256::from(50_000u64),
        }
        .abi_encode()
        .into(),
        token: Address::ZERO,
        min: U256::ZERO,
        writes: vec![],
    };
    assert_eq!(
        fee_transfer_of(&fee, usdc),
        Some((canister, U256::from(50_000u64)))
    );
    let mut on_another_token = fee.clone();
    on_another_token.target = Address::from([0x84; 20]);
    assert_eq!(fee_transfer_of(&on_another_token, usdc), None);
    let mut spends_a_delta = fee.clone();
    spends_a_delta.token = addr(usdc);
    assert_eq!(fee_transfer_of(&spends_a_delta, usdc), None);
    let mut written = fee.clone();
    written.writes.push(Write {
        at: 36,
        minus: U256::ZERO,
    });
    assert_eq!(fee_transfer_of(&written, usdc), None);
    let mut longer = fee;
    let mut data = longer.data.to_vec();
    data.push(0);
    longer.data = data.into();
    assert_eq!(fee_transfer_of(&longer, usdc), None);
}

/// The quoter's fresh fork order (`impl/samples/quoter_order_arbitrum_cctp_gasless.json`):
/// USDC on Arbitrum to WBTC on Polygon over CCTP, gasless through the 3009 door. Its bytes
/// decode, its id recomputes to the one the quoter stored and the 3009 nonce carries, and
/// its steps are the fee and the burn of abi.md 6.1 and 6.2.
#[test]
fn the_real_fork_order_recomputes_its_id() {
    let json: serde_json::Value = serde_json::from_str(include_str!(
        "../../../impl/samples/quoter_order_arbitrum_cctp_gasless.json"
    ))
    .unwrap();
    let text = |field: &str| json[field].as_str().unwrap().to_string();
    let bytes_of = |field: &str| hex::decode(text(field).trim_start_matches("0x")).unwrap();
    let address_of = |field: &str| text(field).parse::<EvmAddress>().unwrap();

    let order = decode_order(&bytes_of("orderAbi")).expect("the stored order decodes");
    assert_eq!(encode_order(&order), bytes_of("orderAbi"));
    let chain = ChainId::new(json["chainId"].as_u64().unwrap());
    let id = order_id(&order, chain, address_of("swapic"));
    assert_eq!(id.to_string(), text("orderId"));
    assert_eq!(id.as_nonce().to_string(), text("nonce"));
    assert_eq!(evm(order.tokenIn), address_of("usdc"));
    assert_eq!(order.amountIn, U256::from(100_000_000u64));

    let usdc = address_of("usdc");
    let (to, fee) = fee_transfer_of(&order.steps[0], usdc).expect("the first step is the fee");
    assert_ne!(to, EvmAddress::ZERO);
    assert_eq!(fee.to_string(), text("networkFee"));
    let messenger: EvmAddress = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
        .parse()
        .unwrap();
    let burn = CctpBurn::of_step(&order.steps[1], messenger)
        .expect("the second step is to the messenger")
        .expect("and is a burn");
    assert_eq!(burn.destination_domain, 7, "Polygon");
    assert_eq!(burn.burn_token, usdc);
    assert_eq!(burn.mint_recipient, burn.destination_caller);
    assert_eq!(hex::encode(&burn.hook_data), &text("envelope")[2..]);
    let envelope = Envelope::decode(&burn.hook_data).unwrap();
    assert_eq!(envelope.action, Action::Swap);
    assert_eq!(envelope.recipient, address_of("recipient"));
    assert_eq!(envelope.deadline, json["arrivalDeadline"].as_u64().unwrap());
    assert!(envelope.min_gas > 0);
    let recipe = decode_recipe(&bytes_of("recipe")).expect("the stored recipe decodes");
    assert_ne!(recipe.target, Address::ZERO);
    assert_ne!(recipe.scaler, Address::ZERO);
}

fn sample_order(json: &str) -> (serde_json::Value, Order) {
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    let order = decode_order(
        &hex::decode(json["orderAbi"].as_str().unwrap().trim_start_matches("0x")).unwrap(),
    )
    .unwrap();
    (json, order)
}

/// The quoter's fresh fork orders on Eco: a gasless CkDeposit from Base to Ethereum, which
/// fills the destination Runner with the envelope, and a user-paid Deliver from Ethereum to
/// Base, which transfers to the user. Both read back to the user as the reward's creator,
/// and to the recipient their route names.
#[test]
fn the_real_eco_steps_read_back_to_their_parties() {
    let (json, order) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_base_eco_ckdeposit_gasless.json"
    ));
    let payer = evm(order.payer);
    let intent = EcoIntent::of_step(&order.steps[1]).unwrap().unwrap();
    assert_eq!(intent.destination, 1);
    assert_eq!(intent.creator, payer);
    let EcoArrival::Fill { runner, envelope } = intent.arrival else {
        panic!("a CkDeposit fills the Runner: {:?}", intent.arrival);
    };
    assert_eq!(runner, json["runner"].as_str().unwrap().parse().unwrap());
    assert_eq!(
        hex::encode(envelope.encode()),
        &json["envelope"].as_str().unwrap()[2..]
    );
    assert_eq!(envelope.action, Action::CkDeposit);
    assert!(
        EcoIntent::of_step(&order.steps[0]).is_none(),
        "the fee is no intent"
    );

    let (_, order) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_ethereum_eco_deliver_userpays.json"
    ));
    let intent = EcoIntent::of_step(&order.steps[1]).unwrap().unwrap();
    assert_eq!(intent.destination, 8453);
    assert_eq!(intent.creator, evm(order.payer));
    assert_eq!(
        intent.arrival,
        EcoArrival::Deliver {
            recipient: evm(order.payer)
        }
    );
    // a route the canister cannot read is refused, not skipped
    let mut garbled = order.steps[1].clone();
    let mut data = garbled.data.to_vec();
    data.truncate(200);
    garbled.data = data.into();
    assert_eq!(EcoIntent::of_step(&garbled), Some(Err(NotAnIntent::Call)));
}

#[test]
fn publish_and_fund_is_the_selector_abi_md_names() {
    assert_eq!(publishAndFundCall::SELECTOR, PUBLISH_AND_FUND);
}

/// The canister reads a step exactly as the contract it calls will: a burn, an intent or a
/// fee whose encoding is not the canonical one (trailing bytes, dirty padding) is not read
/// at all, so no lenient reading can name a party the call does not pay.
#[test]
fn a_lenient_encoding_is_not_read() {
    let (_, order) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_arbitrum_cctp_gasless.json"
    ));
    let messenger: EvmAddress = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
        .parse()
        .unwrap();
    let mut trailing = order.steps[1].clone();
    let mut data = trailing.data.to_vec();
    data.extend_from_slice(&[0; 32]);
    trailing.data = data.into();
    assert!(
        matches!(CctpBurn::of_step(&trailing, messenger), Some(Err(_))),
        "a burn with trailing bytes"
    );
    let (_, eco) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_base_eco_ckdeposit_gasless.json"
    ));
    let mut dirty = eco.steps[1].clone();
    let mut data = dirty.data.to_vec();
    // the high byte of `allowPartial`, a bool: the fourth head word after the selector
    data[4 + 3 * 32] = 1;
    dirty.data = data.into();
    assert!(
        matches!(EcoIntent::of_step(&dirty), Some(Err(_))),
        "an intent with a dirty word"
    );
}

/// The quoter's real KyberSwap step pays the Runner, hands its input only to the executor
/// it calls and pays no fees; the real CkDeposit names its principal in the ck helper's
/// encoding.
#[test]
fn the_real_swap_and_principal_read_back() {
    let (json, order) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_base_local_gasless.json"
    ));
    let swap = KyberSwap::of_step(&order.steps[1]).unwrap().unwrap();
    let runner: EvmAddress = json["runner"].as_str().unwrap().parse().unwrap();
    assert_eq!(swap.dst_receiver, runner);
    assert_eq!(swap.src_token, evm(order.steps[1].token));
    assert_eq!(swap.src_receivers, vec![swap.call_target]);
    assert!(swap.fee_receivers.is_empty() && !swap.permit);
    assert!(KyberSwap::of_step(&order.steps[0]).is_none());

    let (json, _) = sample_order(include_str!(
        "../../../impl/samples/quoter_order_base_eco_ckdeposit_gasless.json"
    ));
    let word: [u8; 32] = hex::decode(json["principal"].as_str().unwrap().trim_start_matches("0x"))
        .unwrap()
        .try_into()
        .unwrap();
    let principal = ck_principal(word).expect("the sample names a principal");
    assert_eq!(principal.as_slice().len(), 10);
    assert_eq!(ck_principal([0; 32]), None, "no principal");
    let mut dirty = word;
    dirty[31] = 1;
    assert_eq!(ck_principal(dirty), None, "bytes past the principal");
}
