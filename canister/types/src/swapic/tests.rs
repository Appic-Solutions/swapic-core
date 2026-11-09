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
        memo: Default::default(),
    }
}

const GOLDEN_SWAPIC: EvmAddress = EvmAddress::new([0xbb; 20]);
const BASE: ChainId = ChainId::BASE;

#[test]
fn order_id_moves_with_every_part_of_the_order() {
    assert_eq!(
        domain(BASE, GOLDEN_SWAPIC).separator(),
        b256!("c395d776d27009143a498a5ebdf6ab4be54d50c85087e1f2c598ceb460bb8b51")
    );
    // any change anywhere is another id, the memo included
    let mut memo = golden();
    memo.memo = bytes!("01");
    assert_ne!(
        order_id(&memo, BASE, GOLDEN_SWAPIC),
        order_id(&golden(), BASE, GOLDEN_SWAPIC)
    );
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
        b256!("391c982619bb5233046c55e62c42b9d3e4c1ec09f980e88f048c95402a61e6ec").0
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
    assert_eq!(sel(selectors::RUN_FOR), "46d6fa96");
    assert_eq!(sel(selectors::COMPLETE_CCTP), "513429dd");
    assert_eq!(sel(selectors::RELEASE), "aa30d53a");
    assert_eq!(sel(selectors::RECEIVE_MESSAGE), "57ecfd28");
    assert_eq!(sel(selectors::TRANSFER), "a9059cbb");
    assert_eq!(sel(selectors::DEPOSIT_FOR_BURN_WITH_HOOK), "779b432d");
    assert_eq!(
        OrderStarted::SIGNATURE_HASH,
        b256!("e95fa9d6e54c817e5526d084398a582163dc771d78687ec4afe3a9665ccb0adc")
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

/// The quoter's fork order (`impl/samples/quoter_order_arbitrum_cctp_gasless.json`, its
/// bytes carried to the memo ABI with an empty memo): USDC on Arbitrum to WBTC on Polygon
/// over CCTP, gasless through the 3009 door. Its bytes decode and re-encode, and its steps
/// are the fee and the burn of abi.md 6.1 and 6.2.
#[test]
fn the_real_fork_order_reads_as_its_steps() {
    let json: serde_json::Value = serde_json::from_str(include_str!(
        "../../../impl/samples/quoter_order_arbitrum_cctp_gasless.json"
    ))
    .unwrap();
    let text = |field: &str| json[field].as_str().unwrap().to_string();
    let bytes_of = |field: &str| hex::decode(text(field).trim_start_matches("0x")).unwrap();
    let address_of = |field: &str| text(field).parse::<EvmAddress>().unwrap();

    let order = decode_order(&bytes_of("orderAbi")).expect("the stored order decodes");
    assert_eq!(encode_order(&order), bytes_of("orderAbi"));
    assert!(order.memo.is_empty());
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

const SAMPLES: [&str; 5] = [
    include_str!("../../../impl/samples/quoter_order_arbitrum_cctp_gasless.json"),
    include_str!("../../../impl/samples/quoter_order_base_eco_ckdeposit_gasless.json"),
    include_str!("../../../impl/samples/quoter_order_base_eco_swap_userpays.json"),
    include_str!("../../../impl/samples/quoter_order_base_local_gasless.json"),
    include_str!("../../../impl/samples/quoter_order_ethereum_eco_deliver_userpays.json"),
];

fn messenger() -> EvmAddress {
    "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"
        .parse()
        .unwrap()
}

/// Parse, rebuild, compare, on the quoter's five real fork orders: every step of every one
/// rebuilds to its own bytes and writes, so the quoter's plans are read as they are.
#[test]
fn every_real_step_round_trips() {
    let mut shapes = Vec::new();
    for text in SAMPLES {
        let (json, order) = sample_order(text);
        let usdc: EvmAddress = json["usdc"].as_str().unwrap().parse().unwrap();
        for step in &order.steps {
            let read = read_step(step, usdc, messenger())
                .unwrap_or_else(|error| panic!("{}: {error}", json["flow"]));
            shapes.push(match read {
                KnownStep::Transfer { .. } => "transfer",
                KnownStep::Burn(_) => "burn",
                KnownStep::Eco { .. } => "eco",
                KnownStep::Swap(_) => "swap",
            });
        }
    }
    assert_eq!(
        shapes,
        ["transfer", "burn", "transfer", "eco", "swap", "eco", "transfer", "swap", "swap", "eco"]
    );
}

/// One refusal per way a step could read differently from what the Runner runs.
#[test]
fn a_step_that_does_not_rebuild_is_refused() {
    let (json, order) = sample_order(SAMPLES[0]);
    let usdc: EvmAddress = json["usdc"].as_str().unwrap().parse().unwrap();
    let burn = order.steps[1].clone();
    let read = |step: &Step| read_step(step, usdc, messenger());
    assert!(matches!(read(&burn), Ok(KnownStep::Burn(_))));

    // a write over the mint recipient
    let mut over = burn.clone();
    over.writes.push(Write {
        at: 4 + 2 * 32,
        minus: U256::ZERO,
    });
    assert_eq!(read(&over), Err(StepError::NotCanonical("writes")));
    // the amount write moved, or its minus changed
    let mut moved = burn.clone();
    moved.writes[0].at = 5;
    assert_eq!(read(&moved), Err(StepError::NotCanonical("writes")));
    // trailing bytes
    let mut trailing = burn.clone();
    let mut data = trailing.data.to_vec();
    data.extend_from_slice(&[0; 32]);
    trailing.data = data.into();
    assert_eq!(read(&trailing), Err(StepError::Decode));
    // a non-canonical offset: the hook data moved one word on, a gap before it
    let mut gapped = burn.clone();
    let mut data = gapped.data.to_vec();
    let at = 4 + 7 * 32;
    let offset = U256::from_be_slice(&data[at..at + 32]);
    data[at..at + 32].copy_from_slice(&(offset + U256::from(32)).to_be_bytes::<32>());
    let tail = 4 + offset.to::<usize>();
    data.splice(tail..tail, [0_u8; 32]);
    gapped.data = data.into();
    assert_eq!(read(&gapped), Err(StepError::Decode));
    // an amount the quoter leaves to the Runner, filled in
    let mut filled = burn.clone();
    let mut data = filled.data.to_vec();
    data[4 + 31] = 1;
    filled.data = data.into();
    assert_eq!(read(&filled), Err(StepError::NotCanonical("calldata")));
    // a step to the messenger that spends another token
    let mut other_token = burn;
    other_token.token = Address::from([0x77; 20]);
    assert_eq!(read(&other_token), Err(StepError::NotCanonical("token")));

    // the Eco step: a write over the route, and the writes' fee not one fee
    let (json, order) = sample_order(SAMPLES[1]);
    let usdc: EvmAddress = json["usdc"].as_str().unwrap().parse().unwrap();
    let eco = order.steps[1].clone();
    let read = |step: &Step| read_step(step, usdc, messenger());
    assert!(matches!(read(&eco), Ok(KnownStep::Eco { .. })));
    let mut over = eco.clone();
    over.writes.push(Write {
        at: 0x200,
        minus: U256::ZERO,
    });
    assert_eq!(read(&over), Err(StepError::NotCanonical("writes")));
    let mut two_fees = eco.clone();
    two_fees.writes[3].minus += U256::from(1);
    assert_eq!(read(&two_fees), Err(StepError::NotCanonical("writes")));
    // a swap step carries no writes at all
    let (json, order) = sample_order(SAMPLES[3]);
    let usdc: EvmAddress = json["usdc"].as_str().unwrap().parse().unwrap();
    let mut swap = order.steps[1].clone();
    swap.writes.push(Write {
        at: 36,
        minus: U256::ZERO,
    });
    assert_eq!(
        read_step(&swap, usdc, messenger()),
        Err(StepError::NotCanonical("writes"))
    );
    // and a step of no shape is no step the canister reads
    let mut unknown = order.steps[1].clone();
    unknown.data = vec![0xde, 0xad, 0xbe, 0xef].into();
    assert_eq!(
        read_step(&unknown, usdc, messenger()),
        Err(StepError::Unknown)
    );
}

/// `usdc` under Circle's usual domain, "USD Coin" version "2": every chain the canister
/// launched on.
fn usd_coin(usdc: EvmAddress) -> UsdcDomain<'static> {
    UsdcDomain {
        address: usdc,
        name: crate::config::DEFAULT_USDC_NAME,
        version: crate::config::DEFAULT_USDC_VERSION,
    }
}

/// The three doors' digests over the interface golden order, computed with cast outside
/// this crate: the 3009 authorization (its nonce the order id), the Permit2 witness transfer
/// and the 2612 door's QuoteAcceptance. What the app has its user sign.
#[test]
fn the_door_digests_are_the_cast_goldens() {
    let golden = interface();
    let golden = &golden["order"];
    let order = interface_order(&golden["value"]);
    let chain = ChainId::new(golden["chain_id"].as_u64().unwrap());
    let swapic: EvmAddress = text(&golden["swapic"]).parse().unwrap();
    let doors = &golden["doors"];
    let id = order_id(&order, chain, swapic);
    let usdc: EvmAddress = text(&doors["auth_3009"]["domain"]["verifyingContract"])
        .parse()
        .unwrap();
    let auth = GaslessAuth {
        door: DOOR_AUTH_3009,
        validAfter: U256::ZERO,
        validBefore: uint(&doors["auth_3009"]["value"]["validBefore"]),
        nonce: id.as_nonce(),
        ..Default::default()
    };
    assert_eq!(
        receive_with_authorization_digest(&order, &auth, id, chain, usd_coin(usdc), swapic),
        word(&doors["auth_3009"]["digest"])
    );
    let permit2 = GaslessAuth {
        door: DOOR_PERMIT2,
        nonce: uint(&doors["permit2"]["value"]["nonce"]),
        deadline: order.deadline,
        ..Default::default()
    };
    assert_eq!(
        permit2_digest(&order, &permit2, id, chain, swapic),
        word(&doors["permit2"]["digest"])
    );
    assert_eq!(
        acceptance_digest(&order, chain, swapic),
        word(&doors["acceptance_2612"]["digest"])
    );
}

/// The topic0 of every event a pool arrival is proven by, as the real mainnet receipts in
/// `impl/src/pool/arrival/samples` carry them (2026-10-19 staging canaries): OrderStarted
/// and PaidOut on Base, MessageReceived of Base's MessageTransmitterV2, IntentPublished
/// on Arbitrum's Eco Portal, and IntentFulfilled on Base's (the fill's log 0x5c; its other
/// Portal log, 0x6c with topic 0xe6d8040a, is not one the canister reads).
#[test]
fn the_arrival_topics_are_the_chains_own() {
    let hex = |topic: [u8; 32]| hex::encode(topic);
    assert_eq!(
        hex(topics::order_started()),
        "e95fa9d6e54c817e5526d084398a582163dc771d78687ec4afe3a9665ccb0adc"
    );
    assert_eq!(
        hex(topics::paid_out()),
        "fe625474f74d7d56170a110187c9c1821d944870487a41d606d217e47871eb89"
    );
    assert_eq!(
        hex(topics::transfer()),
        "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
    );
    assert_eq!(
        hex(topics::message_received()),
        "ff48c13eda96b1cceacc6b9edeedc9e9db9d6226afbc30146b720c19d3addb1c"
    );
    assert_eq!(
        hex(topics::intent_published()),
        "43974895be1bcec7344337863fa7de24a0d1c315c0a994f663fe0ee220ddc8e4"
    );
    assert_eq!(
        hex(topics::intent_fulfilled()),
        "c471de166a60c0b81727dfa2f57d4fc3ad1b45b057c1f034b7058365613bde8d"
    );
    // CCTP v2's `MessageSent(bytes)`, Circle's own topic
    assert_eq!(
        hex(topics::message_sent()),
        "8c5261668696ce22758910d05bab8f186d6eb247ceac2af2e82c7dc17669b036"
    );
    // the Arbitrum Portal's funding log, 0x13
    assert_eq!(
        hex(topics::intent_funded()),
        "c1ed05721d27ad6b2555d61388ac393b120f5cc0e6009e53230e02c68e60064a"
    );
}

/// KyberSwap's InputScalingHelper as the deployed Runner calls it: its address, and the
/// selector of `getScaledInputData(bytes,uint256)`.
#[test]
fn the_scaler_is_kybers_input_scaling_helper() {
    assert_eq!(
        KYBER_SCALER,
        "0x2f577A41BeC1BE1152AeEA12e73b7391d15f655D"
            .parse::<EvmAddress>()
            .unwrap()
    );
    assert_eq!(getScaledInputDataCall::SELECTOR, [0xe4, 0xda, 0x6f, 0x2e]);
}

/// The books' balance read: ERC-20 `balanceOf(address)`, the account in its word.
#[test]
fn a_balance_read_is_erc20_balance_of() {
    let account: EvmAddress = "0x36D34038F04990a58c068e08eD9754e2E3D9bA53"
        .parse()
        .unwrap();
    assert_eq!(
        hex::encode(balance_of_calldata(account)),
        "70a08231\
         00000000000000000000000036d34038f04990a58c068e08ed9754e2e3d9ba53"
    );
}

const INTERFACE: &str = include_str!("../../../golden/interface.json");

fn interface() -> serde_json::Value {
    serde_json::from_str(INTERFACE).expect("the golden file is JSON")
}

fn text(value: &serde_json::Value) -> &str {
    value.as_str().unwrap()
}

fn hex_bytes(value: &serde_json::Value) -> Vec<u8> {
    hex::decode(text(value).trim_start_matches("0x")).unwrap()
}

fn word(value: &serde_json::Value) -> [u8; 32] {
    hex_bytes(value).try_into().unwrap()
}

fn uint(value: &serde_json::Value) -> U256 {
    match value {
        serde_json::Value::String(text) => U256::from_str_radix(text, 10).unwrap(),
        number => U256::from(number.as_u64().unwrap()),
    }
}

fn address_of(value: &serde_json::Value) -> Address {
    text(value).parse().unwrap()
}

/// The golden file's order, read field by field.
fn interface_order(value: &serde_json::Value) -> Order {
    Order {
        r#ref: FixedBytes(word(&value["ref"])),
        payer: address_of(&value["payer"]),
        tokenIn: address_of(&value["tokenIn"]),
        amountIn: uint(&value["amountIn"]),
        deadline: uint(&value["deadline"]),
        steps: value["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| Step {
                target: address_of(&step["target"]),
                data: hex_bytes(&step["data"]).into(),
                token: address_of(&step["token"]),
                min: uint(&step["min"]),
                writes: step["writes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|write| Write {
                        at: u32::try_from(write["at"].as_u64().unwrap()).unwrap(),
                        minus: uint(&write["minus"]),
                    })
                    .collect(),
            })
            .collect(),
        payout: Payout {
            token: address_of(&value["payout"]["token"]),
            recipient: address_of(&value["payout"]["recipient"]),
            minOut: uint(&value["payout"]["minOut"]),
        },
        memo: hex_bytes(&value["memo"]).into(),
    }
}

/// The interface golden order, computed with cast outside this crate: its `abi.encode`
/// bytes, its EIP-712 type string and type hash, the domain separator, its struct hash and
/// its id, and the `OrderStarted` topic. The quoter, the watcher, the app and the contracts
/// are held to the same file.
#[test]
fn the_interface_golden_order_holds() {
    let golden = interface();
    let golden = &golden["order"];
    let order = interface_order(&golden["value"]);
    let chain = ChainId::new(golden["chain_id"].as_u64().unwrap());
    let swapic: EvmAddress = text(&golden["swapic"]).parse().unwrap();
    assert_eq!(text(&golden["type_string"]), ORDER_TYPE);
    assert_eq!(keccak256(ORDER_TYPE).0, word(&golden["type_hash"]));
    assert_eq!(
        domain(chain, swapic).separator().0,
        word(&golden["domain_separator"])
    );
    assert_eq!(encode_order(&order), hex_bytes(&golden["abi_encode"]));
    assert_eq!(
        decode_order(&hex_bytes(&golden["abi_encode"])),
        Some(order.clone())
    );
    assert_eq!(hash_order(&order), word(&golden["struct_hash"]));
    assert_eq!(
        order_id(&order, chain, swapic).into_bytes(),
        word(&golden["order_id"])
    );
    assert_eq!(
        topics::order_started(),
        word(&golden["order_started_topic0"])
    );
    let selector = |field: &str| text(&golden["selectors"][field]).to_string();
    assert_eq!(
        format!("0x{}", hex::encode(selectors::RUN_FOR)),
        selector("runFor")
    );
    assert_eq!(
        format!("0x{}", hex::encode(runCall::SELECTOR)),
        selector("run")
    );
    assert_eq!(
        keccak256(text(&golden["order_started_signature"])).0,
        topics::order_started()
    );
}

/// An `OrderStarted` log reads back with its memo: what the canister's own Base scan and
/// the watcher take a deposit's terms from.
#[test]
fn an_order_started_log_reads_back_with_its_memo() {
    let golden = interface();
    let order = interface_order(&golden["order"]["value"]);
    let id = [0x5e; 32];
    let event = OrderStarted {
        id: FixedBytes(id),
        r#ref: order.r#ref,
        payer: order.payer,
        tokenIn: order.tokenIn,
        amountIn: order.amountIn,
        memo: order.memo.clone(),
    };
    let topics: Vec<[u8; 32]> = event
        .encode_topics()
        .iter()
        .map(|topic| topic.0 .0)
        .collect();
    let data = event.encode_data();
    assert_eq!(topics[0], topics::order_started());
    assert_eq!(
        read_order_started(&topics, &data),
        Some(StartedOrder {
            id: OrderId::new(id),
            r#ref: order.r#ref.0,
            payer: evm(order.payer),
            token_in: evm(order.tokenIn),
            amount_in: order.amountIn,
            memo: order.memo.to_vec(),
        })
    );
    assert_eq!(read_order_started(&topics[..3], &data), None);
}

/// The interface golden order with an empty memo, the most common case, computed with cast
/// outside this crate: `keccak256(0x)` is the memo's word in its struct hash.
#[test]
fn the_interface_golden_empty_memo_order_holds() {
    let golden = interface();
    let golden = &golden["order_empty_memo"];
    let order = interface_order(&golden["value"]);
    assert!(order.memo.is_empty());
    assert_eq!(keccak256([]).0, word(&golden["empty_memo_hash"]));
    let chain = ChainId::new(golden["chain_id"].as_u64().unwrap());
    let swapic: EvmAddress = text(&golden["swapic"]).parse().unwrap();
    assert_eq!(
        domain(chain, swapic).separator().0,
        word(&golden["domain_separator"])
    );
    assert_eq!(encode_order(&order), hex_bytes(&golden["abi_encode"]));
    assert_eq!(hash_order(&order), word(&golden["struct_hash"]));
    assert_eq!(
        order_id(&order, chain, swapic).into_bytes(),
        word(&golden["order_id"])
    );
}

/// The message is the first argument of every finish, whatever its calldata carries after it.
#[test]
fn finish_message_reads_the_first_argument_of_each_finish() {
    let message = vec![0xab; 696];
    let attestation = vec![0xcd; 130];
    let recipe = Recipe {
        target: alloy_primitives::Address::ZERO,
        data: alloy_primitives::Bytes::new(),
        scaler: alloy_primitives::Address::ZERO,
    };
    for calldata in [
        receive_message_calldata(&message, &attestation),
        complete_cctp_calldata(&message, &attestation, recipe),
        release_calldata(&message, &attestation),
    ] {
        assert_eq!(finish_message(&calldata), Some(message.as_slice()));
    }
}

/// Calldata too short or pointing past its end reads as nothing, and never panics.
#[test]
fn finish_message_refuses_what_does_not_read() {
    assert_eq!(finish_message(&[]), None);
    assert_eq!(finish_message(&[0; 35]), None);
    let mut past_the_end = vec![0; 4];
    past_the_end.extend_from_slice(&[0xff; 32]);
    assert_eq!(finish_message(&past_the_end), None);
    let mut long = vec![0; 4];
    long.extend_from_slice(&U256::from(32u8).to_be_bytes::<32>());
    long.extend_from_slice(&U256::from(1_000u32).to_be_bytes::<32>());
    assert_eq!(finish_message(&long), None);
}
