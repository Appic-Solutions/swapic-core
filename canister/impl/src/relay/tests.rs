use super::*;
use crate::storage::events::{append_event_at, read_state};
use crate::storage::on_fresh_memory;
use std::collections::BTreeMap;
use types::chain_data::ChainReading;
use types::config::PerAction;
use types::swapic::{Step, Write};
use types::{BlockNumber, EventType, FlowStep, Nonce, TxHash, WeiPerGas};

const ARBITRUM: ChainId = ChainId::ARBITRUM;
const POLYGON: ChainId = ChainId::POLYGON;

/// One of the quoter's fresh fork orders in `samples/`, with the gas limit the quoter
/// charged its fee on, and the chains it names configured as the fork deploy has them.
struct Sample {
    json: serde_json::Value,
    chain_id: ChainId,
    order: Order,
    auth: GaslessAuth,
    /// The canister the fee step pays: the fork deploy's.
    canister: EvmAddress,
    evm_chains: ChainTable<EvmChain>,
    domains: ChainTable<CctpDomain>,
    /// The source limit the quoter charged for.
    gas: u64,
}

fn address(text: &str) -> EvmAddress {
    text.parse().unwrap()
}

const MESSENGER: &str = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d";
const ECO_PORTAL: &str = "0xEC000769A73b70e16f361a442292500b3BCf4A85";

/// A chain the canister signs on, every action given two million gas, a budget of 0.01
/// ether and no least fee unless a test sets one.
fn chain(
    swapic: EvmAddress,
    runner: EvmAddress,
    receiver: EvmAddress,
    usdc: EvmAddress,
) -> EvmChain {
    EvmChain {
        swapic,
        runner,
        cctp_receiver: receiver,
        usdc,
        token_messenger: address(MESSENGER),
        message_transmitter: address("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"),
        gas_limit: PerAction::all(GasAmount::from(2_000_000_u64)),
        gas_budget: PerAction::all(Wei::from(10_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
    }
}

/// `samples/quoter_order_arbitrum_cctp_gasless.json`: 100 USDC on Arbitrum to WBTC on
/// Polygon over CCTP, gasless through the 3009 door.
fn sample() -> Sample {
    load(include_str!(
        "../../samples/quoter_order_arbitrum_cctp_gasless.json"
    ))
}

/// `samples/quoter_order_base_eco_ckdeposit_gasless.json`: 50 USDC on Base to ckUSDC
/// through Eco to Ethereum's ck helper, gasless.
fn eco_sample() -> Sample {
    load(include_str!(
        "../../samples/quoter_order_base_eco_ckdeposit_gasless.json"
    ))
}

/// `samples/quoter_order_base_local_gasless.json`: 100 USDC to WETH on Base, gasless,
/// paid out to the user.
fn local_sample() -> Sample {
    load(include_str!(
        "../../samples/quoter_order_base_local_gasless.json"
    ))
}

/// The fork deploy's three contracts, at one address on every chain (one deployer, one
/// nonce), and each chain's real USDC.
fn fork_chains() -> ChainTable<EvmChain> {
    let swapic = address("0xF2E9d13Ba690c71cfF7788DE7273Ca7658d6D7C7");
    let runner = address("0xd9a6d4123BcF737A36e5f66D474891f15E8d2d3C");
    // the CctpReceiver the real CCTP order burns to: Swapic's first child
    let receiver = {
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../samples/quoter_order_arbitrum_cctp_gasless.json"
        ))
        .unwrap();
        let order = swapic::decode_order(
            &hex::decode(json["orderAbi"].as_str().unwrap().trim_start_matches("0x")).unwrap(),
        )
        .unwrap();
        let burn = CctpBurn::of_step(&order.steps[1], address(MESSENGER))
            .unwrap()
            .unwrap();
        swapic::evm_of_word(burn.mint_recipient).unwrap()
    };
    ChainTable(BTreeMap::from([
        (
            ChainId::ETHEREUM,
            chain(
                swapic,
                runner,
                receiver,
                address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            ),
        ),
        (
            ChainId::BASE,
            chain(
                swapic,
                runner,
                receiver,
                address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
            ),
        ),
        (
            ARBITRUM,
            chain(
                swapic,
                runner,
                receiver,
                address("0xaf88d065e77c8cC2239327C5EDb3A432268e5831"),
            ),
        ),
        (
            POLYGON,
            chain(
                swapic,
                runner,
                receiver,
                address("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),
            ),
        ),
    ]))
}

fn load(text: &str) -> Sample {
    let json: serde_json::Value = serde_json::from_str(text).unwrap();
    let text = |field: &str| json[field].as_str().unwrap_or_default().to_string();
    let chain_id = ChainId::new(json["chainId"].as_u64().unwrap());
    let order =
        swapic::decode_order(&hex::decode(text("orderAbi").trim_start_matches("0x")).unwrap())
            .unwrap();
    // a user-paid order has no authorization: these are zero for it
    let number = |field: &str| text(field).parse::<U256>().unwrap_or_default();
    let auth = GaslessAuth {
        door: DOOR_AUTH_3009,
        validAfter: U256::ZERO,
        validBefore: number("validBefore"),
        nonce: number("nonce"),
        deadline: U256::ZERO,
        signature: vec![0x1b; 65].into(),
        acceptance: Default::default(),
    };
    let usdc = address(&text("usdc"));
    let canister = swapic::fee_transfer_of(&order.steps[0], usdc)
        .map_or(EvmAddress::new([0xca; 20]), |(to, _)| to);
    assert_eq!(fork_chains().get(chain_id).unwrap().usdc, usdc);
    let domains = ChainTable(BTreeMap::from([
        (ChainId::ETHEREUM, CctpDomain::new(0)),
        (ARBITRUM, CctpDomain::new(3)),
        (ChainId::BASE, CctpDomain::new(6)),
        (POLYGON, CctpDomain::new(7)),
    ]));
    let gas = json["sourceGasLimit"].as_u64().unwrap();
    Sample {
        json,
        chain_id,
        order,
        auth,
        canister,
        evm_chains: fork_chains(),
        domains,
        gas,
    }
}

impl Sample {
    fn deadline(&self) -> u64 {
        self.json["deadline"].as_str().unwrap().parse().unwrap()
    }

    /// A clock a few minutes before the order's deadline.
    fn now(&self) -> u64 {
        self.deadline() - 120
    }

    fn check(&self) -> Result<Relay, RelayError> {
        self.check_with(&self.order, &self.auth)
    }

    fn check_with(&self, order: &Order, auth: &GaslessAuth) -> Result<Relay, RelayError> {
        let chains = Chains {
            chain_id: self.chain_id,
            evm_chains: &self.evm_chains,
            domains: &self.domains,
            canister: self.canister,
            eco_portal: Some(address(ECO_PORTAL)),
        };
        check(order, auth, self.gas, &chains, self.now(), 3_600)
    }

    /// `order` signed again: the 3009 nonce is its own id, so only the rule under test
    /// can refuse it.
    fn resigned(&self, order: &Order) -> GaslessAuth {
        let swapic = self.evm_chains.get(self.chain_id).unwrap().swapic;
        GaslessAuth {
            nonce: swapic::order_id(order, self.chain_id, swapic).as_nonce(),
            ..self.auth.clone()
        }
    }

    /// `order` checked as signed again.
    fn check_resigned(&self, order: &Order) -> Result<Relay, RelayError> {
        self.check_with(order, &self.resigned(order))
    }

    fn set_min_fee(&mut self, chain_id: ChainId, action: GasAction, fee: u64) {
        let mut chain = self.evm_chains.get(chain_id).unwrap();
        let fee = TokenAmount::from(fee);
        match action {
            GasAction::RelayCctp => chain.min_network_fee.relay_cctp = fee,
            GasAction::CompleteSwap => chain.min_network_fee.complete_swap = fee,
            _ => unreachable!("the sample needs only these two"),
        }
        self.evm_chains.0.insert(chain_id, chain);
    }
}

#[test]
fn the_real_order_is_relayed_as_the_quoter_built_it() {
    let mut sample = sample();
    // the relay on Arbitrum and the completion on Polygon, inside the 166,275 charged
    sample.set_min_fee(ARBITRUM, GasAction::RelayCctp, 100_000);
    sample.set_min_fee(POLYGON, GasAction::CompleteSwap, 60_000);
    let relay = sample.check().unwrap();
    assert_eq!(
        relay.order_id.to_string(),
        sample.json["orderId"].as_str().unwrap()
    );
    assert_eq!(relay.action, GasAction::RelayCctp);
    assert_eq!(relay.fee, TokenAmount::from(166_275_u64));
    assert_eq!(
        relay.arrival,
        Some(Arrival {
            chain_id: POLYGON,
            action: GasAction::CompleteSwap,
            recipient: address(sample.json["recipient"].as_str().unwrap()),
            parties: vec![Party::Evm(address(
                sample.json["recipient"].as_str().unwrap()
            ))],
        })
    );
    // exactly the limit the quoter charged for
    assert_eq!(relay.gas_limit, GasAmount::from(393_566_u64));
}

/// Plan Review Focus: an order that pays the canister no fee step, two of them, or too
/// little for what the canister signs for it is refused, whoever asks.
#[test]
fn fee_step_required() {
    let mut sample = sample();
    let fee = sample.order.steps[0].clone();
    let burn = sample.order.steps[1].clone();

    let mut no_fee = sample.order.clone();
    no_fee.steps = vec![burn.clone()];
    assert_eq!(
        sample.check_with(&no_fee, &sample.resigned(&no_fee)),
        Err(RelayError::NoFeeStep)
    );

    let mut two_fees = sample.order.clone();
    two_fees.steps = vec![fee.clone(), fee.clone(), burn.clone()];
    assert_eq!(
        sample.check_with(&two_fees, &sample.resigned(&two_fees)),
        Err(RelayError::FeeSteps(2))
    );

    // the same transfer to anyone else is no fee to the canister, and a step the canister
    // does not read
    let mut elsewhere = sample.order.clone();
    let mut data = fee.data.to_vec();
    data[16..36].copy_from_slice(&[0x99; 20]);
    elsewhere.steps[0].data = data.into();
    assert_eq!(
        sample.check_with(&elsewhere, &sample.resigned(&elsewhere)),
        Err(RelayError::UnknownStep(0))
    );

    // a transfer with a write is not the fixed fee: the write could land on its recipient
    let mut written = sample.order.clone();
    written.steps[0].writes = vec![Write {
        at: 36,
        minus: U256::ZERO,
    }];
    assert_eq!(
        sample.check_with(&written, &sample.resigned(&written)),
        Err(RelayError::Step {
            index: 0,
            error: StepError::NotCanonical("token, min or writes")
        })
    );

    // a CCTP order pays the relay AND the completion on the destination
    sample.set_min_fee(ARBITRUM, GasAction::RelayCctp, 100_000);
    sample.set_min_fee(POLYGON, GasAction::CompleteSwap, 66_276);
    assert_eq!(
        sample.check(),
        Err(RelayError::FeeBelowMinimum {
            paid: TokenAmount::from(166_275_u64),
            needed: TokenAmount::from(166_276_u64),
        })
    );
    sample.set_min_fee(POLYGON, GasAction::CompleteSwap, 66_275);
    assert!(sample.check().is_ok());
}

/// Plan Review Focus: the canister recomputes the id as Swapic does, so an authorization
/// for another order, for this order on another Swapic, or for an order changed after it
/// was signed, is refused before anything is signed.
#[test]
fn wrong_order_id_refused() {
    let sample = sample();
    let id = sample.check().unwrap().order_id;

    let mut another = sample.auth.clone();
    another.nonce += U256::from(1);
    assert_eq!(
        sample.check_with(&sample.order, &another),
        Err(RelayError::WrongOrderId(id))
    );

    let mut tampered = sample.order.clone();
    tampered.amountIn += U256::from(1);
    assert!(matches!(
        sample.check_with(&tampered, &sample.auth),
        Err(RelayError::WrongOrderId(other)) if other != id
    ));

    let mut elsewhere = self::sample();
    let mut arbitrum = elsewhere.evm_chains.get(ARBITRUM).unwrap();
    arbitrum.swapic = EvmAddress::new([0x77; 20]);
    elsewhere.evm_chains.0.insert(ARBITRUM, arbitrum);
    assert!(matches!(
        elsewhere.check(),
        Err(RelayError::WrongOrderId(other)) if other != id
    ));

    // and a chain the canister does not sign on is refused by name
    let chains = Chains {
        chain_id: ChainId::new(10),
        evm_chains: &sample.evm_chains,
        domains: &sample.domains,
        canister: sample.canister,
        eco_portal: Some(address(ECO_PORTAL)),
    };
    assert_eq!(
        check(
            &sample.order,
            &sample.auth,
            sample.gas,
            &chains,
            sample.now(),
            3_600
        ),
        Err(RelayError::UnknownChain(ChainId::new(10)))
    );
}

#[test]
fn the_deadline_must_leave_time_to_land_and_not_be_far_ahead() {
    let sample = sample();
    let chains = Chains {
        chain_id: ARBITRUM,
        evm_chains: &sample.evm_chains,
        domains: &sample.domains,
        canister: sample.canister,
        eco_portal: Some(address(ECO_PORTAL)),
    };
    let at = |now: u64, window: u64| {
        check(
            &sample.order,
            &sample.auth,
            sample.gas,
            &chains,
            now,
            window,
        )
    };
    let deadline = sample.deadline();
    assert!(at(deadline - RELAY_MARGIN_S - 1, 3_600).is_ok());
    assert!(matches!(
        at(deadline - RELAY_MARGIN_S, 3_600),
        Err(RelayError::Deadline { .. })
    ));
    assert!(matches!(
        at(deadline - 120, 60),
        Err(RelayError::Deadline { .. })
    ));
}

#[test]
fn a_burn_whose_arrival_cannot_be_finished_is_refused() {
    let sample = sample();
    let messenger = address("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d");
    let rebuilt = |edit: &dyn Fn(&mut types::swapic::depositForBurnWithHookCall)| {
        use alloy_sol_types::SolCall;
        let mut call =
            types::swapic::depositForBurnWithHookCall::abi_decode(&sample.order.steps[1].data)
                .unwrap();
        edit(&mut call);
        let mut order = sample.order.clone();
        order.steps[1] = Step {
            data: call.abi_encode().into(),
            ..order.steps[1].clone()
        };
        sample.check_with(&order, &sample.resigned(&order))
    };
    assert!(CctpBurn::of_step(&sample.order.steps[1], messenger).is_some());
    // minted to anyone but the destination receiver: the receiver refuses the message
    assert!(matches!(
        rebuilt(&|call| call.mintRecipient = [0x99; 32].into()),
        Err(RelayError::Burn(_))
    ));
    // a caller-zero burn to the receiver: anyone could strand it there
    assert!(matches!(
        rebuilt(&|call| call.destinationCaller = [0; 32].into()),
        Err(RelayError::Burn(_))
    ));
    // a domain the canister finishes nothing on (Unichain's)
    assert_eq!(
        rebuilt(&|call| call.destinationDomain = 10),
        Err(RelayError::UnknownDestination(10))
    );
    // an envelope the receiver will not decode
    assert!(matches!(
        rebuilt(&|call| {
            let mut hook = call.hookData.to_vec();
            hook[31] = 2;
            call.hookData = hook.into();
        }),
        Err(RelayError::Envelope(_))
    ));
}

/// Plan Review Focus: the same order relayed twice signs once. The fold refuses a second
/// nonce while the first is out and a second record once it is signed, and the door
/// answers the first record's id.
#[test]
fn idempotent_per_order() {
    on_fresh_memory(|| {
        crate::storage::init();
        let at = Timestamp::from_nanos(1);
        let key = QuoteHash::new(sample().check().unwrap().order_id.into_bytes());
        let created = |nonce: u64| EventType::TxCreated {
            purpose: TxPurpose::Relay(key),
            chain_id: ARBITRUM,
            nonce: Nonce::new(nonce),
            to: EvmAddress::new([0xf2; 20]),
            value: Wei::ZERO,
            data: types::swapic::selectors::RUN_FOR.to_vec(),
            gas_limit: GasAmount::from(393_566_u64),
            max_fee: WeiPerGas::from(20_000_000_u64),
            max_priority_fee: WeiPerGas::from(1_000_000_u64),
            action: None,
        };
        assert_eq!(tx::signed_once(key), Ok(None));
        append_event_at(created(0), at).unwrap();
        assert!(tx::signed_once(key).is_err(), "being signed");
        assert!(append_event_at(created(1), at).is_err(), "no second nonce");
        let id = append_event_at(
            EventType::Flow {
                quote_hash: key,
                step: FlowStep::GaslessRelayed {
                    chain_id: ARBITRUM,
                    nonce: Nonce::new(0),
                    tx_hash: TxHash::new([1; 32]),
                    raw_tx: vec![0x02, 0xf8],
                },
            },
            at,
        )
        .unwrap()
        .get();
        assert_eq!(tx::signed_once(key), Ok(Some(id)));
        assert!(append_event_at(created(1), at).is_err(), "no second relay");
        let flow = read_state(|state| state.flow(&key)).unwrap();
        assert_eq!(flow.signed_id, Some(id));
    });
}

/// Plan Review Focus: the relay is signed with exactly the limit the quoter charged for,
/// held to the chain's maximum, and a fee spike that puts it over the action's budget
/// leaves it unsigned.
#[test]
fn gas_rule_refuses_over_budget() {
    let sample = sample();
    let relay = sample.check().unwrap();
    let arbitrum = sample.evm_chains.get(ARBITRUM).unwrap();
    // a larger limit is refused, a smaller one signed exactly
    assert!(matches!(
        signing_gas(ARBITRUM, &arbitrum, GasAction::RelayCctp, 5_000_000),
        Err(RelayError::GasAboveMax { .. })
    ));
    assert_eq!(
        signing_gas(ARBITRUM, &arbitrum, GasAction::RelayCctp, 20_999),
        Err(RelayError::GasLimitTooLow(20_999))
    );
    let fees = |base_fee: u64| {
        ChainReading {
            block: BlockNumber::new(1),
            base_fee: WeiPerGas::from(base_fee),
            priority_fee: WeiPerGas::from(10_000_000_u64),
        }
        .pushed_at(Timestamp::from_nanos(1))
        .fees()
        .unwrap()
    };
    let config = types::Config {
        evm_chains: sample.evm_chains.clone(),
        ..types::Config::default()
    };
    // 0.02 gwei on Arbitrum: 393,566 gas costs far less than the 0.01 ether budget
    assert_eq!(
        tx::gas_rule(
            &config,
            ARBITRUM,
            relay.action,
            fees(20_000_000),
            relay.gas_limit
        ),
        Ok(())
    );
    // a spike to 20 gwei: 40.01 gwei times 393,566 gas is above it
    assert!(matches!(
        tx::gas_rule(
            &config,
            ARBITRUM,
            relay.action,
            fees(20_000_000_000),
            relay.gas_limit
        ),
        Err(tx::TxError::OverGasBudget { .. })
    ));
}

// The sanctions screen: every party the canister can read in an order is screened, and an
// order whose recipient it cannot read is not signed at all.

use alloy_sol_types::{SolCall, SolValue};
use types::swapic::{
    depositForBurnWithHookCall, fillCall, publishAndFundCall, selectors, transferCall, EcoCall,
    EcoRoute, NotAnIntent, Payout,
};

/// Someone the samples never name.
const SOMEONE: EvmAddress = EvmAddress::new([0x42; 20]);

fn sanctioned(address: EvmAddress) {
    crate::storage::sanctions::apply(&[types::Address::from(address)], &[]).unwrap();
}

/// The party the screen stops `order` at, the order signed again and otherwise admitted.
fn stopped_at(sample: &Sample, order: &Order) -> Option<Party> {
    sanctioned_party(&sample.check_resigned(order).unwrap().parties)
}

/// `order` with its CCTP burn edited.
fn with_burn(order: &Order, edit: impl Fn(&mut depositForBurnWithHookCall)) -> Order {
    let mut order = order.clone();
    let step = order
        .steps
        .iter_mut()
        .find(|step| swapic::selector_of(step) == Some(selectors::DEPOSIT_FOR_BURN_WITH_HOOK))
        .unwrap();
    let mut call = depositForBurnWithHookCall::abi_decode(&step.data).unwrap();
    edit(&mut call);
    step.data = call.abi_encode().into();
    order
}

/// `order` with its Eco intent edited, the route included, and its writes rebuilt for
/// the edited route as the quoter would write them, when it still reads.
fn with_eco(order: &Order, edit: impl Fn(&mut publishAndFundCall, &mut EcoRoute)) -> Order {
    let mut order = order.clone();
    let step = order
        .steps
        .iter_mut()
        .find(|step| swapic::selector_of(step) == Some(swapic::PUBLISH_AND_FUND))
        .unwrap();
    let mut call = publishAndFundCall::abi_decode(&step.data).unwrap();
    let mut route = <EcoRoute as SolValue>::abi_decode(&call.route).unwrap();
    edit(&mut call, &mut route);
    let route_len = SolValue::abi_encode(&route).len();
    call.route = SolValue::abi_encode(&route).into();
    step.data = call.abi_encode().into();
    if let Some(Ok(intent)) = EcoIntent::of_step(step) {
        step.writes = swapic::eco_writes(&intent.arrival, route_len, step.writes[1].minus);
    }
    order
}

#[test]
fn a_sanctioned_payer_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = sample();
        assert_eq!(stopped_at(&sample, &sample.order), None);
        let payer = swapic::evm(sample.order.payer);
        sanctioned(payer);
        assert_eq!(stopped_at(&sample, &sample.order), Some(Party::Evm(payer)));
    });
}

#[test]
fn a_sanctioned_cctp_recipient_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = sample();
        let order = with_burn(&sample.order, |call| {
            let mut envelope = Envelope::decode(&call.hookData).unwrap();
            envelope.recipient = SOMEONE;
            call.hookData = envelope.encode().into();
        });
        assert_eq!(stopped_at(&sample, &order), None);
        sanctioned(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), Some(Party::Evm(SOMEONE)));
    });
}

#[test]
fn a_sanctioned_cctp_deliver_mint_recipient_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = sample();
        let order = with_burn(&sample.order, |call| {
            let mut envelope = Envelope::decode(&call.hookData).unwrap();
            envelope.action = Action::Deliver;
            envelope.recipient = SOMEONE;
            envelope.min_gas = 0;
            call.hookData = envelope.encode().into();
            call.mintRecipient = SOMEONE.to_word().into();
            call.destinationCaller = [0; 32].into();
        });
        let relay = sample.check_resigned(&order).unwrap();
        assert_eq!(relay.arrival.unwrap().action, GasAction::RelayDeliver);
        assert_eq!(stopped_at(&sample, &order), None);
        sanctioned(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), Some(Party::Evm(SOMEONE)));
    });
}

#[test]
fn a_sanctioned_payout_recipient_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = local_sample();
        let mut order = sample.order.clone();
        order.payout.recipient = swapic::addr(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), None);
        sanctioned(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), Some(Party::Evm(SOMEONE)));
    });
}

/// The creator of an Eco intent is who an unfilled intent refunds: it is the payer, so
/// the payer's screen covers it, and any other creator is refused before any screen.
#[test]
fn an_eco_creator_other_than_the_payer_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = eco_sample();
        let payer = swapic::evm(sample.order.payer);
        let order = with_eco(&sample.order, |call, _| {
            call.reward.creator = swapic::addr(SOMEONE);
        });
        assert_eq!(
            sample.check_resigned(&order),
            Err(RelayError::Eco("refunds to someone other than the payer"))
        );
        sanctioned(payer);
        assert_eq!(stopped_at(&sample, &sample.order), Some(Party::Evm(payer)));
    });
}

#[test]
fn a_sanctioned_eco_fill_recipient_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = eco_sample();
        let order = with_eco(&sample.order, |_, route| {
            let mut fill = fillCall::abi_decode(&route.calls[1].data).unwrap();
            let mut envelope = Envelope::decode(&fill.envelope).unwrap();
            envelope.recipient = SOMEONE;
            fill.envelope = envelope.encode().into();
            route.calls[1].data = fill.abi_encode().into();
        });
        assert_eq!(stopped_at(&sample, &order), None);
        sanctioned(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), Some(Party::Evm(SOMEONE)));
    });
}

#[test]
fn a_sanctioned_eco_transfer_recipient_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = eco_sample();
        let order = with_eco(&sample.order, |_, route| {
            let token = route.tokens[0].token;
            route.calls = vec![EcoCall {
                target: token,
                data: transferCall {
                    to: swapic::addr(SOMEONE),
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
                value: U256::ZERO,
            }];
        });
        assert_eq!(stopped_at(&sample, &order), None);
        sanctioned(SOMEONE);
        assert_eq!(stopped_at(&sample, &order), Some(Party::Evm(SOMEONE)));
    });
}

/// The canister signs only plans it fully reads: a recipient it cannot find in a fee step,
/// a CCTP burn, an Eco intent or a payout is a recipient it cannot screen.
#[test]
fn a_plan_whose_recipient_cannot_be_read_is_refused() {
    let local = local_sample();
    assert!(local.check().is_ok());
    let mut nobody = local.order.clone();
    nobody.payout = Payout::default();
    assert_eq!(local.check_resigned(&nobody), Err(RelayError::NoRecipient));

    let eco = eco_sample();
    assert!(eco.check().is_ok());
    let unread = with_eco(&eco.order, |_, route| {
        let extra = route.calls[0].clone();
        route.calls.push(extra);
    });
    assert_eq!(
        eco.check_resigned(&unread),
        Err(RelayError::Step {
            index: 1,
            error: StepError::Intent(NotAnIntent::Calls)
        })
    );
}

// Gaps in the screen, each shown on a real order before it was closed.

alloy_sol_types::sol! {
    /// KyberSwap's MetaAggregationRouterV2, spelled here independently of the canister's
    /// own reading of it.
    struct KyberDescription {
        address srcToken;
        address dstToken;
        address[] srcReceivers;
        uint256[] srcAmounts;
        address[] feeReceivers;
        uint256[] feeAmounts;
        address dstReceiver;
        uint256 amount;
        uint256 minReturnAmount;
        uint256 flags;
        bytes permit;
    }
    struct KyberExecution {
        address callTarget;
        address approveTarget;
        bytes targetData;
        KyberDescription desc;
        bytes clientData;
    }
    function swap(KyberExecution execution) external payable returns (uint256, uint256);
}

/// `order` with its KyberSwap step edited.
fn with_swap(order: &Order, edit: impl Fn(&mut KyberExecution)) -> Order {
    let mut order = order.clone();
    let step = order
        .steps
        .iter_mut()
        .find(|step| swapic::selector_of(step) == Some(swapCall::SELECTOR))
        .unwrap();
    let mut call = swapCall::abi_decode(&step.data).unwrap();
    edit(&mut call.execution);
    step.data = call.abi_encode().into();
    order
}

/// A step beside the known shapes (the fee, a CCTP burn, an Eco intent, a KyberSwap swap
/// paying the Runner) moves money the screen never sees: here a plain USDC transfer to
/// someone, next to a fee and a burn that read clean.
#[test]
fn a_step_the_canister_cannot_read_is_refused() {
    let sample = sample();
    let usdc = sample.evm_chains.get(ARBITRUM).unwrap().usdc;
    let mut order = sample.order.clone();
    order.steps.insert(
        1,
        Step {
            target: swapic::addr(usdc),
            data: transferCall {
                to: swapic::addr(SOMEONE),
                amount: U256::from(1_000_000),
            }
            .abi_encode()
            .into(),
            token: alloy_primitives::Address::ZERO,
            min: U256::ZERO,
            writes: vec![],
        },
    );
    assert_eq!(
        sample.check_resigned(&order),
        Err(RelayError::UnknownStep(1))
    );
}

/// A swap's own receivers are parties too: its output must go to the Runner, its input to
/// the receivers its route starts at but never to the zero address, Swapic's contracts or
/// the canister, and nothing to fee receivers.
#[test]
fn a_swap_that_pays_anyone_but_the_runner_is_refused() {
    let sample = local_sample();
    assert!(sample.check().is_ok());
    let to_someone = with_swap(&sample.order, |execution| {
        execution.desc.dstReceiver = swapic::addr(SOMEONE);
    });
    assert_eq!(
        sample.check_resigned(&to_someone),
        Err(RelayError::Swap("pays its output to other than the Runner"))
    );
    let fee_to_someone = with_swap(&sample.order, |execution| {
        execution.desc.feeReceivers = vec![swapic::addr(SOMEONE)];
        execution.desc.feeAmounts = vec![U256::from(1)];
    });
    assert_eq!(
        sample.check_resigned(&fee_to_someone),
        Err(RelayError::Swap("pays fees or carries a permit"))
    );
    // a pool the route starts at may take the input; the Runner's minimums hold the rest
    let input_to_a_pool = with_swap(&sample.order, |execution| {
        execution.desc.srcReceivers = vec![swapic::addr(SOMEONE)];
    });
    assert!(sample.check_resigned(&input_to_a_pool).is_ok());
    let runner = sample.evm_chains.get(sample.chain_id).unwrap().runner;
    let input_to_the_runner = with_swap(&sample.order, |execution| {
        execution.desc.srcReceivers = vec![swapic::addr(runner)];
    });
    assert_eq!(
        sample.check_resigned(&input_to_the_runner),
        Err(RelayError::Swap(
            "hands its input to the zero address, one of Swapic's own contracts or the canister"
        ))
    );
}

/// KyberSwap's router calldata as the quoter built it for the Arbitrum Runner on 2026-11-01
/// (`samples/kyber_arb_weth_usdc_*_2026_11_01.json`), as a step spending WETH.
fn kyber_capture(json: &str) -> Step {
    let capture: serde_json::Value = serde_json::from_str(json).unwrap();
    let data = capture["build"]["data"]["data"].as_str().unwrap();
    let weth: EvmAddress = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
        .parse()
        .unwrap();
    Step {
        target: swapic::addr(KYBER_ROUTER),
        data: hex::decode(data.trim_start_matches("0x")).unwrap().into(),
        token: swapic::addr(weth),
        min: capture["build"]["data"]["amountIn"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
        writes: vec![],
    }
}

fn pool_capture() -> Step {
    kyber_capture(include_str!(
        "../../samples/kyber_arb_weth_usdc_pool_2026_11_01.json"
    ))
}

fn executor_capture() -> Step {
    kyber_capture(include_str!(
        "../../samples/kyber_arb_weth_usdc_executor_2026_11_01.json"
    ))
}

/// `step` read and checked as `read_plan` reads and checks a swap step on Arbitrum.
fn checked_swap(sample: &Sample, step: &Step) -> Result<(), RelayError> {
    let source = sample.evm_chains.get(ARBITRUM).unwrap();
    match swapic::read_step(step, source.usdc, source.token_messenger) {
        Ok(KnownStep::Swap(swap)) => check_swap(&swap, step, &source, sample.canister),
        other => panic!("not a swap step: {other:?}"),
    }
}

/// `step` with its swap edited, encoded again canonically.
fn edited(step: &Step, edit: impl Fn(&mut KyberExecution)) -> Step {
    let mut call = swapCall::abi_decode(&step.data).unwrap();
    edit(&mut call.execution);
    Step {
        data: call.abi_encode().into(),
        ..step.clone()
    }
}

/// The two shapes KyberSwap builds: a route starting at an old v2-style pool, which the
/// router pays itself (0.001 WETH to USDC through a SushiSwap v2 pool), and one starting at
/// the executor (0.005 WETH through PancakeSwap v3). Both pay the Arbitrum Runner, and both
/// are read and pass.
#[test]
fn a_swap_starting_at_a_pool_or_at_the_executor_is_taken() {
    let sample = sample();
    let source = sample.evm_chains.get(ARBITRUM).unwrap();
    let runner: EvmAddress = "0xd9a6d4123BcF737A36e5f66D474891f15E8d2d3C"
        .parse()
        .unwrap();
    assert_eq!(
        source.runner, runner,
        "the captures were built for this Runner"
    );
    let sushi: EvmAddress = "0x57b85fef094e10b5eecdf350af688299e9553378"
        .parse()
        .unwrap();
    for (step, first) in [
        (pool_capture(), sushi),
        (executor_capture(), KYBER_EXECUTOR),
    ] {
        let Ok(KnownStep::Swap(swap)) =
            swapic::read_step(&step, source.usdc, source.token_messenger)
        else {
            panic!("the capture reads as a swap step");
        };
        assert_eq!(swap.src_receivers, vec![first]);
        assert_eq!(swap.call_target, KYBER_EXECUTOR);
        assert_eq!(swap.dst_receiver, runner);
        assert_eq!(checked_swap(&sample, &step), Ok(()));
    }
}

/// Everything but the input's first receiver is held as before, and the receivers are
/// held to at least one, none the zero address, Swapic, the Runner, the CctpReceiver or the
/// canister: each refused shape, on the real pool capture.
#[test]
fn a_swap_handing_its_input_to_us_or_paying_elsewhere_is_refused() {
    let sample = sample();
    let source = sample.evm_chains.get(ARBITRUM).unwrap();
    let step = pool_capture();
    let receivers = |receivers: Vec<EvmAddress>| {
        edited(&step, |execution| {
            execution.desc.srcAmounts = receivers.iter().map(|_| U256::from(1)).collect();
            execution.desc.srcReceivers = receivers.iter().map(|r| swapic::addr(*r)).collect();
        })
    };
    let ours = "hands its input to the zero address, one of Swapic's own contracts or the canister";
    assert_eq!(
        checked_swap(&sample, &receivers(vec![])),
        Err(RelayError::Swap("hands its input to no receiver"))
    );
    let sushi: EvmAddress = "0x57b85fef094e10b5eecdf350af688299e9553378"
        .parse()
        .unwrap();
    for bad in [
        EvmAddress::ZERO,
        source.swapic,
        source.runner,
        source.cctp_receiver,
        sample.canister,
    ] {
        assert_eq!(
            checked_swap(&sample, &receivers(vec![bad])),
            Err(RelayError::Swap(ours)),
            "{bad}"
        );
        assert_eq!(
            checked_swap(&sample, &receivers(vec![sushi, bad])),
            Err(RelayError::Swap(ours)),
            "{bad} beside the pool"
        );
    }
    assert_eq!(
        checked_swap(&sample, &receivers(vec![sushi, SOMEONE])),
        Ok(())
    );
    let fees = edited(&step, |execution| {
        execution.desc.feeReceivers = vec![swapic::addr(SOMEONE)];
        execution.desc.feeAmounts = vec![U256::from(1)];
    });
    assert_eq!(
        checked_swap(&sample, &fees),
        Err(RelayError::Swap("pays fees or carries a permit"))
    );
    let permit = edited(&step, |execution| {
        execution.desc.permit = vec![1, 2, 3].into()
    });
    assert_eq!(
        checked_swap(&sample, &permit),
        Err(RelayError::Swap("pays fees or carries a permit"))
    );
    let elsewhere = edited(&step, |execution| {
        execution.desc.dstReceiver = swapic::addr(SOMEONE)
    });
    assert_eq!(
        checked_swap(&sample, &elsewhere),
        Err(RelayError::Swap("pays its output to other than the Runner"))
    );
    let other_executor = edited(&step, |execution| {
        execution.callTarget = swapic::addr(SOMEONE)
    });
    assert_eq!(
        checked_swap(&sample, &other_executor),
        Err(RelayError::Swap("calls other than KyberSwap's executor"))
    );
    let other_token = Step {
        token: swapic::addr(source.usdc),
        ..step.clone()
    };
    assert_eq!(
        checked_swap(&sample, &other_token),
        Err(RelayError::Swap("does not sell the token the step spends"))
    );
    let other_router = Step {
        target: swapic::addr(SOMEONE),
        ..step.clone()
    };
    assert_eq!(
        checked_swap(&sample, &other_router),
        Err(RelayError::Swap("is not on KyberSwap's router"))
    );
}

/// An Eco intent is read as the destination will run it only when the canister knows the
/// destination: its USDC, and its Runner as the one approved and filled. Its refunds go to
/// its creator, who is the payer.
#[test]
fn an_eco_intent_the_canister_cannot_bind_is_refused() {
    let sample = eco_sample();
    assert!(sample.check().is_ok());
    let mut unknown = eco_sample();
    unknown.evm_chains.0.remove(&ChainId::ETHEREUM);
    assert_eq!(unknown.check(), Err(RelayError::EcoUnknownDestination(1)));
    let other_creator = with_eco(&sample.order, |call, _| {
        call.reward.creator = swapic::addr(SOMEONE);
    });
    assert_eq!(
        sample.check_resigned(&other_creator),
        Err(RelayError::Eco("refunds to someone other than the payer"))
    );
    // a route whose approve and token agree, on a token that is not the destination's USDC
    let other_token = with_eco(&sample.order, |_, route| {
        route.tokens[0].token = swapic::addr(SOMEONE);
        route.calls[0].target = swapic::addr(SOMEONE);
        let mut fill = fillCall::abi_decode(&route.calls[1].data).unwrap();
        fill.token = swapic::addr(SOMEONE);
        route.calls[1].data = fill.abi_encode().into();
    });
    assert_eq!(
        sample.check_resigned(&other_token),
        Err(RelayError::Eco("carries other than the destination's USDC"))
    );
    let partial = with_eco(&sample.order, |call, _| call.allowPartial = true);
    assert_eq!(
        sample.check_resigned(&partial),
        Err(RelayError::Step {
            index: 1,
            error: StepError::Intent(NotAnIntent::Partial)
        })
    );
    let elsewhere = {
        let mut order = sample.order.clone();
        order.steps[1].target = swapic::addr(SOMEONE);
        order
    };
    assert_eq!(
        sample.check_resigned(&elsewhere),
        Err(RelayError::Eco("is not published on Eco's Portal"))
    );
}

/// A CkDeposit pays an ICP principal: it is a party like any address.
#[test]
fn a_sanctioned_ck_principal_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        let sample = eco_sample();
        let relay = sample.check().unwrap();
        assert!(sanctioned_party(&relay.parties).is_none());
        // the sample's principal, length-prefixed in its envelope
        let word: [u8; 32] = hex::decode(
            sample.json["principal"]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
        )
        .unwrap()
        .try_into()
        .unwrap();
        let principal = candid::Principal::from_slice(&word[1..1 + word[0] as usize]);
        crate::storage::sanctions::apply(&[principal.to_text().parse().unwrap()], &[]).unwrap();
        let relay = sample.check().unwrap();
        assert!(sanctioned_party(&relay.parties).is_some());
    });
}

/// The Runner applies every write after the screen has read the step, so a write over a
/// screened word replaces it at run time: over a burn's mint recipient, over an Eco
/// route's recipient, over a swap's output receiver. Writes are exactly each shape's own.
#[test]
fn a_write_over_a_screened_word_is_refused() {
    let cctp = sample();
    // depositForBurnWithHook's third word, the mint recipient: the Runner would write its
    // delta minus `minus` there, which can be any address an attacker solves for
    let mut over_mint = cctp.order.clone();
    over_mint.steps[1].writes.push(Write {
        at: 4 + 2 * 32,
        minus: U256::ZERO,
    });
    let writes = RelayError::Step {
        index: 1,
        error: StepError::NotCanonical("writes"),
    };
    assert_eq!(cctp.check_resigned(&over_mint), Err(writes.clone()));

    let eco = eco_sample();
    let mut over_route = eco.order.clone();
    over_route.steps[1].writes.push(Write {
        at: 0x200,
        minus: U256::ZERO,
    });
    assert_eq!(eco.check_resigned(&over_route), Err(writes.clone()));
    let mut moved = eco.order.clone();
    moved.steps[1].writes[1].at += 32;
    assert_eq!(eco.check_resigned(&moved), Err(writes.clone()));

    let local = local_sample();
    let mut over_receiver = local.order.clone();
    over_receiver.steps[1].writes.push(Write {
        at: 36,
        minus: U256::ZERO,
    });
    assert_eq!(local.check_resigned(&over_receiver), Err(writes));
}

/// A step whose calldata is not the canonical encoding of its shape is refused: trailing
/// bytes, and an offset that leaves a gap, each of which a lax decoder reads past.
#[test]
fn a_step_not_canonically_encoded_is_refused() {
    let sample = sample();
    let decode = Err(RelayError::Step {
        index: 1,
        error: StepError::Decode,
    });
    let mut trailing = sample.order.clone();
    let mut data = trailing.steps[1].data.to_vec();
    data.extend_from_slice(&[0; 32]);
    trailing.steps[1].data = data.into();
    assert_eq!(sample.check_resigned(&trailing), decode);

    let mut gapped = sample.order.clone();
    let mut data = gapped.steps[1].data.to_vec();
    let at = 4 + 7 * 32;
    let offset = U256::from_be_slice(&data[at..at + 32]);
    data[at..at + 32].copy_from_slice(&(offset + U256::from(32)).to_be_bytes::<32>());
    let tail = 4 + offset.to::<usize>();
    data.splice(tail..tail, [0_u8; 32]);
    gapped.steps[1].data = data.into();
    assert_eq!(sample.check_resigned(&gapped), decode);
}

/// One fee pays for one arrival: an order that burns twice is refused.
#[test]
fn a_second_burn_is_refused() {
    let sample = sample();
    let mut twice = sample.order.clone();
    twice.steps.push(twice.steps[1].clone());
    assert_eq!(sample.check_resigned(&twice), Err(RelayError::Burns(2)));
}

/// The quoter's five real fork orders, as they are: each reads as the Runner will run it
/// (both gas modes), names its payer and its recipient, and each gasless one is relayed.
#[test]
fn every_real_order_reads_as_the_quoter_built_it() {
    let samples = [
        (sample(), GasAction::RelayCctp, true),
        (eco_sample(), GasAction::RelayEco, true),
        (local_sample(), GasAction::RelayLocal, true),
        (
            load(include_str!(
                "../../samples/quoter_order_base_eco_swap_userpays.json"
            )),
            GasAction::RelayEco,
            false,
        ),
        (
            load(include_str!(
                "../../samples/quoter_order_ethereum_eco_deliver_userpays.json"
            )),
            GasAction::RelayEco,
            false,
        ),
    ];
    for (sample, action, gasless) in samples {
        let flow = sample.json["flow"].as_str().unwrap().to_string();
        let chains = Chains {
            chain_id: sample.chain_id,
            evm_chains: &sample.evm_chains,
            domains: &sample.domains,
            canister: sample.canister,
            eco_portal: Some(address(ECO_PORTAL)),
        };
        let plan = read_plan(&sample.order, &chains).unwrap_or_else(|e| panic!("{flow}: {e}"));
        assert_eq!(plan.action, action, "{flow}");
        assert_eq!(plan.fees.len(), usize::from(gasless), "{flow}");
        let payer = Party::Evm(swapic::evm(sample.order.payer));
        assert_eq!(plan.parties[0], payer, "{flow}");
        assert!(plan.parties.len() >= 2, "{flow}: the recipient is read");
        if gasless {
            assert!(sample.check().is_ok(), "{flow}");
        }
    }
}

/// Review fix 1: an authorization must still hold when the relay lands, so `validBefore`
/// and a Permit2 `deadline` are at least the landing margin ahead, and `validAfter` has
/// begun.
#[test]
fn the_authorization_must_outlast_the_landing_margin() {
    let sample = sample();
    let now = sample.now();
    let with = |edit: &dyn Fn(&mut GaslessAuth)| {
        let mut auth = sample.auth.clone();
        edit(&mut auth);
        sample.check_with(&sample.order, &auth)
    };
    assert!(with(&|_| {}).is_ok());
    let short = with(&|auth| auth.validBefore = U256::from(now + RELAY_MARGIN_S - 1));
    assert_eq!(
        short,
        Err(RelayError::Authorization("expires before the relay lands"))
    );
    assert!(with(&|auth| auth.validBefore = U256::from(now + RELAY_MARGIN_S)).is_ok());
    let early = with(&|auth| auth.validAfter = U256::from(now + 1));
    assert_eq!(early, Err(RelayError::Authorization("is not valid yet")));
    assert!(with(&|auth| auth.validAfter = U256::from(now)).is_ok());

    let permit2 = |deadline: u64| {
        with(&|auth| {
            auth.door = DOOR_PERMIT2;
            auth.deadline = U256::from(deadline);
        })
    };
    assert_eq!(
        permit2(now + RELAY_MARGIN_S - 1),
        Err(RelayError::Authorization(
            "Permit2 permit expires before the relay lands"
        ))
    );
    assert!(permit2(now + RELAY_MARGIN_S).is_ok());
}

/// Review fix 2: a transaction is never signed below the gas its fee was charged on. A
/// limit above the chain's maximum is refused for every action, but a release, which
/// anyone may send and which the watcher asks for with the arrival's limit, is signed at
/// the maximum.
#[test]
fn a_gas_limit_above_the_maximum_is_refused() {
    let sample = sample();
    let arbitrum = sample.evm_chains.get(ARBITRUM).unwrap();
    for action in [
        GasAction::RelayCctp,
        GasAction::RelayEco,
        GasAction::RelayLocal,
        GasAction::CompleteSwap,
        GasAction::CompleteCk,
        GasAction::RelayDeliver,
    ] {
        assert_eq!(
            signing_gas(ARBITRUM, &arbitrum, action, 2_000_001),
            Err(RelayError::GasAboveMax {
                action,
                requested: 2_000_001,
                max: GasAmount::from(2_000_000_u64),
            })
        );
        assert_eq!(
            signing_gas(ARBITRUM, &arbitrum, action, 2_000_000),
            Ok(GasAmount::from(2_000_000_u64))
        );
    }
    assert_eq!(
        signing_gas(ARBITRUM, &arbitrum, GasAction::Release, 5_000_000),
        Ok(GasAmount::from(2_000_000_u64))
    );
}

/// A payer's key, for tests that sign as one.
fn payer_key() -> libsecp256k1::SecretKey {
    libsecp256k1::SecretKey::parse(&[0x11; 32]).unwrap()
}

fn address_of_key(key: &libsecp256k1::SecretKey) -> EvmAddress {
    let public = libsecp256k1::PublicKey::from_secret_key(key).serialize();
    EvmAddress::from_public_key(&public)
}

/// `r ++ s ++ v`, `v` 27 or 28, as a wallet signs.
fn sign(digest: [u8; 32], key: &libsecp256k1::SecretKey) -> Vec<u8> {
    let (signature, recovery) = libsecp256k1::sign(&libsecp256k1::Message::parse(&digest), key);
    let mut out = signature.serialize().to_vec();
    out.push(recovery.serialize() + 27);
    out
}

/// Review fix 5: each door's signature is recovered over exactly what the door verifies,
/// and must be the payer's own.
#[test]
fn the_payers_authorization_is_verified_on_every_door() {
    let sample = sample();
    let chain = sample.evm_chains.get(ARBITRUM).unwrap();
    let key = payer_key();
    let mut order = sample.order.clone();
    order.payer = swapic::addr(address_of_key(&key));
    let id = swapic::order_id(&order, ARBITRUM, chain.swapic);
    let mut auth = sample.resigned(&order);
    let authorize = |auth: &GaslessAuth| authorize(&order, auth, id, ARBITRUM, &chain);

    // 3009: USDC's ReceiveWithAuthorization under the chain's USDC
    auth.signature = sign(
        swapic::receive_with_authorization_digest(
            &order,
            &auth,
            id,
            ARBITRUM,
            chain.usdc_domain(),
            chain.swapic,
        ),
        &key,
    )
    .into();
    assert_eq!(authorize(&auth), Ok(()));
    let mut later = auth.clone();
    later.validBefore += U256::from(1);
    assert_eq!(authorize(&later), Err(RelayError::ContractPayerUnsupported));
    let stranger = libsecp256k1::SecretKey::parse(&[0x22; 32]).unwrap();
    let mut theirs = auth.clone();
    theirs.signature = sign(
        swapic::receive_with_authorization_digest(
            &order,
            &auth,
            id,
            ARBITRUM,
            chain.usdc_domain(),
            chain.swapic,
        ),
        &stranger,
    )
    .into();
    assert_eq!(
        authorize(&theirs),
        Err(RelayError::ContractPayerUnsupported)
    );
    // the same signature with s mirrored to the curve's high half recovers too, and is
    // refused as USDC's ECRecover refuses it
    let mut high = auth.clone();
    high.signature = high_s(&auth.signature).into();
    assert_eq!(
        authorize(&high),
        Err(RelayError::Authorization("signature is not in low-s form"))
    );

    // 2612: the payer's QuoteAcceptance of the order id
    let accepted = GaslessAuth {
        door: DOOR_PERMIT_2612,
        acceptance: sign(
            swapic::acceptance_digest(&order, ARBITRUM, chain.swapic),
            &key,
        )
        .into(),
        ..auth.clone()
    };
    assert_eq!(authorize(&accepted), Ok(()));
    // a contract wallet's ERC-1271 signature is no ECDSA signature of the payer's key
    let contract = GaslessAuth {
        acceptance: vec![0x1b; 65].into(),
        ..accepted.clone()
    };
    assert_eq!(
        authorize(&contract),
        Err(RelayError::ContractPayerUnsupported)
    );

    // Permit2: the witness transfer with the order id as the witness
    let mut permit2 = GaslessAuth {
        door: DOOR_PERMIT2,
        nonce: U256::from(7),
        deadline: order.deadline,
        ..auth.clone()
    };
    permit2.signature = sign(
        swapic::permit2_digest(&order, &permit2, id, ARBITRUM, chain.swapic),
        &key,
    )
    .into();
    assert_eq!(authorize(&permit2), Ok(()));
    let mut other_nonce = permit2.clone();
    other_nonce.nonce = U256::from(8);
    assert_eq!(
        authorize(&other_nonce),
        Err(RelayError::ContractPayerUnsupported)
    );

    // the 3009 door pulls only the chain's USDC, whose domain it is signed under
    let mut weth = order.clone();
    weth.tokenIn = swapic::addr(EvmAddress::new([0x82; 20]));
    assert!(matches!(
        authorize_order(&weth, &auth, &chain),
        Err(RelayError::Authorization(_))
    ));
}

fn authorize_order(order: &Order, auth: &GaslessAuth, chain: &EvmChain) -> Result<(), RelayError> {
    let id = swapic::order_id(order, ARBITRUM, chain.swapic);
    authorize(order, auth, id, ARBITRUM, chain)
}

/// `r ++ s ++ v` of a signature, its `s` mirrored to the curve's high half and `v`
/// flipped: the same signer, the malleable twin.
fn high_s(signature: &[u8]) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    let s = U256::from_be_slice(&bytes[32..64]);
    let n = U256::from_str_radix(
        "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
        16,
    )
    .unwrap();
    bytes[32..64].copy_from_slice(&(n - s).to_be_bytes::<32>());
    bytes[64] = if bytes[64] == 27 { 28 } else { 27 };
    bytes
}

/// EIP-2098's 64 bytes: `r`, then `s` with `v`'s parity in its top bit.
fn compact(signature: &[u8]) -> Vec<u8> {
    let mut bytes = signature[..64].to_vec();
    if signature[64] == 28 {
        bytes[32] |= 0x80;
    }
    bytes
}

/// Security round 5: each door accepts exactly the signatures its verifier accepts, no
/// more (the canister would pay for the revert) and no less:
/// - 3009, USDC's ECRecover: 65 bytes, `v` 27 or 28, `s` in the low half;
/// - 2612, the acceptance through OpenZeppelin's `tryRecover`: the same;
/// - Permit2's SignatureVerification: 65 bytes or EIP-2098's 64, `v` 27 or 28, `s` either
///   half, since it calls `ecrecover` as it is.
///
/// And every field of a digest is the parsed order's: a signature over another value,
/// chain or verifying contract recovers to someone else.
#[test]
fn each_door_accepts_exactly_what_its_verifier_accepts() {
    let sample = sample();
    let chain = sample.evm_chains.get(ARBITRUM).unwrap();
    let key = payer_key();
    let mut order = sample.order.clone();
    order.payer = swapic::addr(address_of_key(&key));
    let id = swapic::order_id(&order, ARBITRUM, chain.swapic);
    let auth = sample.resigned(&order);
    let authorize = |auth: &GaslessAuth| authorize(&order, auth, id, ARBITRUM, &chain);
    let rwa = |order: &Order, chain_id: ChainId, usdc: EvmAddress| {
        let domain = swapic::UsdcDomain {
            address: usdc,
            ..chain.usdc_domain()
        };
        swapic::receive_with_authorization_digest(order, &auth, id, chain_id, domain, chain.swapic)
    };
    let with_signature = |auth: &GaslessAuth, signature: Vec<u8>| GaslessAuth {
        signature: signature.into(),
        ..auth.clone()
    };
    let v_zero_one = |signature: &[u8]| {
        let mut bytes = signature.to_vec();
        bytes[64] -= 27;
        bytes
    };

    // 3009
    let good = sign(rwa(&order, ARBITRUM, chain.usdc), &key);
    assert_eq!(authorize(&with_signature(&auth, good.clone())), Ok(()));
    assert_eq!(
        authorize(&with_signature(&auth, v_zero_one(&good))),
        Err(RelayError::Authorization("signature's v is not 27 or 28"))
    );
    assert_eq!(
        authorize(&with_signature(&auth, high_s(&good))),
        Err(RelayError::Authorization("signature is not in low-s form"))
    );
    assert_eq!(
        authorize(&with_signature(&auth, compact(&good))),
        Err(RelayError::Authorization("signature is not 65 bytes"))
    );
    let mut more = order.clone();
    more.amountIn += U256::from(1);
    for (what, digest) in [
        (
            "a value other than amountIn",
            rwa(&more, ARBITRUM, chain.usdc),
        ),
        ("another chain id", rwa(&order, ChainId::BASE, chain.usdc)),
        (
            "another verifying contract",
            rwa(&order, ARBITRUM, EvmAddress::new([0x83; 20])),
        ),
    ] {
        assert_eq!(
            authorize(&with_signature(&auth, sign(digest, &key))),
            Err(RelayError::ContractPayerUnsupported),
            "{what}"
        );
    }

    // 2612: the acceptance, under Swapic's domain on this chain
    let accepted = sign(
        swapic::acceptance_digest(&order, ARBITRUM, chain.swapic),
        &key,
    );
    let door_2612 = |acceptance: Vec<u8>| GaslessAuth {
        door: DOOR_PERMIT_2612,
        acceptance: acceptance.into(),
        ..auth.clone()
    };
    assert_eq!(authorize(&door_2612(accepted.clone())), Ok(()));
    assert_eq!(
        authorize(&door_2612(v_zero_one(&accepted))),
        Err(RelayError::Authorization("signature's v is not 27 or 28"))
    );
    assert_eq!(
        authorize(&door_2612(high_s(&accepted))),
        Err(RelayError::Authorization("signature is not in low-s form"))
    );
    assert_eq!(
        authorize(&door_2612(compact(&accepted))),
        Err(RelayError::Authorization("signature is not 65 bytes"))
    );
    for (what, digest) in [
        (
            "another chain id",
            swapic::acceptance_digest(&order, ChainId::BASE, chain.swapic),
        ),
        (
            "another verifying contract",
            swapic::acceptance_digest(&order, ARBITRUM, EvmAddress::new([0x77; 20])),
        ),
    ] {
        assert_eq!(
            authorize(&door_2612(sign(digest, &key))),
            Err(RelayError::ContractPayerUnsupported),
            "{what}"
        );
    }

    // Permit2: either half of s, and the compact form
    let permit = GaslessAuth {
        door: DOOR_PERMIT2,
        nonce: U256::from(7),
        deadline: order.deadline,
        ..auth.clone()
    };
    let signed = sign(
        swapic::permit2_digest(&order, &permit, id, ARBITRUM, chain.swapic),
        &key,
    );
    assert_eq!(authorize(&with_signature(&permit, signed.clone())), Ok(()));
    assert_eq!(authorize(&with_signature(&permit, high_s(&signed))), Ok(()));
    assert_eq!(
        authorize(&with_signature(&permit, compact(&signed))),
        Ok(())
    );
    assert_eq!(
        authorize(&with_signature(&permit, v_zero_one(&signed))),
        Err(RelayError::Authorization("signature's v is not 27 or 28"))
    );
    assert_eq!(
        authorize(&with_signature(&permit, signed[..63].to_vec())),
        Err(RelayError::Authorization("signature is not 65 or 64 bytes"))
    );
    let mut cheaper = order.clone();
    cheaper.amountIn -= U256::from(1);
    let other = sign(
        swapic::permit2_digest(&cheaper, &permit, id, ARBITRUM, chain.swapic),
        &key,
    );
    assert_eq!(
        authorize(&with_signature(&permit, other)),
        Err(RelayError::ContractPayerUnsupported),
        "an amount other than amountIn"
    );
}

/// What both methods do with the reading they were given, right after their guards: none
/// leaves the stored one as it is (review focus 1); a valid one is stored and fresh at once;
/// an invalid one refuses with the push's text and is not stored.
#[test]
fn a_call_stores_the_reading_it_carries() {
    on_fresh_memory(|| {
        crate::storage::init();
        let chain = ChainId::new(137);
        let at = Timestamp::from_nanos(1_700_000_000_000_000_000);
        assert_eq!(take_reading(chain, None, at), Ok(()));
        assert!(chain_data::get(chain).is_none());
        let reading = ChainData {
            base_fee_wei_per_gas: candid::Nat::from(250_000_000_000u64),
            block: 95_798_869,
            priority_fee_wei_per_gas: candid::Nat::from(30_000_000_000u64),
        };
        assert_eq!(take_reading(chain, Some(reading.clone()), at), Ok(()));
        assert_eq!(
            chain_data::get(chain).map(|d| d.block.get()),
            Some(95_798_869)
        );
        // 2^256, the first fee no 256-bit price holds
        let too_large = candid::Nat::parse(
            b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
        )
        .unwrap();
        let invalid = ChainData {
            block: 95_798_900,
            priority_fee_wei_per_gas: too_large,
            ..reading
        };
        assert_eq!(
            take_reading(chain, Some(invalid), at),
            Err(r#"InvalidData(FeeTooLarge { field: "priority_fee_wei_per_gas" })"#.to_string())
        );
        assert_eq!(
            chain_data::get(chain).map(|d| d.block.get()),
            Some(95_798_869)
        );
    });
}

/// Review fix: a carried reading no newer than the stored one is kept out. A retry resends
/// the reading it was first priced on, and a slow call can land after a newer push; either
/// would otherwise replace newer fees and be stamped fresh. It is validated all the same,
/// and a newer one replaces the stored one.
#[test]
fn a_carried_reading_no_newer_than_the_stored_one_is_kept_out() {
    on_fresh_memory(|| {
        crate::storage::init();
        let chain = ChainId::ETHEREUM;
        let first = Timestamp::from_nanos(1_700_000_000_000_000_000);
        let later = Timestamp::from_nanos(1_700_000_005_000_000_000);
        let at_block = |block: u64| ChainData {
            base_fee_wei_per_gas: candid::Nat::from(2_000_000_000u64),
            block,
            priority_fee_wei_per_gas: candid::Nat::from(100_000_000u64),
        };
        let stored = || chain_data::get(chain).map(|d| (d.block.get(), d.pushed_at));
        assert_eq!(take_reading(chain, Some(at_block(100)), first), Ok(()));
        assert_eq!(stored(), Some((100, first)));
        // an older block: the stored reading stays, stamp and all
        assert_eq!(take_reading(chain, Some(at_block(90)), later), Ok(()));
        assert_eq!(stored(), Some((100, first)));
        // the same block: nothing newer, so the stamp is not moved either
        assert_eq!(take_reading(chain, Some(at_block(100)), later), Ok(()));
        assert_eq!(stored(), Some((100, first)));
        // kept out, and still refused when invalid
        let invalid = ChainData {
            base_fee_wei_per_gas: candid::Nat::parse(
                b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
            )
            .unwrap(),
            ..at_block(90)
        };
        assert_eq!(
            take_reading(chain, Some(invalid), later),
            Err(r#"InvalidData(FeeTooLarge { field: "base_fee_wei_per_gas" })"#.to_string())
        );
        assert_eq!(stored(), Some((100, first)));
        // a newer block replaces it
        assert_eq!(take_reading(chain, Some(at_block(101)), later), Ok(()));
        assert_eq!(stored(), Some((101, later)));
    });
}

/// Anvil's first account: a published test key that holds nothing, the signer of the two
/// real 3009 signatures below.
const ANVIL: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
/// The staging deploy's Swapic, one address on every chain.
const STAGING_SWAPIC: &str = "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406";

/// A gasless order of 5 USDC on `usdc`, paid by Anvil's first account.
fn anvil_order(usdc: EvmAddress) -> Order {
    let anvil: EvmAddress = ANVIL.parse().unwrap();
    Order {
        r#ref: alloy_primitives::FixedBytes([0x5a; 32]),
        payer: swapic::addr(anvil),
        tokenIn: swapic::addr(usdc),
        amountIn: U256::from(5_000_000_u64),
        deadline: U256::from(1_793_189_700_u64),
        steps: vec![],
        payout: swapic::Payout {
            token: swapic::addr(usdc),
            recipient: swapic::addr(
                "0x2222222222222222222222222222222222222222"
                    .parse()
                    .unwrap(),
            ),
            minOut: U256::from(4_900_000_u64),
        },
    }
}

/// The 3009 door is checked under the chain's own USDC domain. Two real signatures, made
/// with `cast wallet sign --data` (foundry 1.3.5) from Anvil's first key over the typed data
/// of USDC's `ReceiveWithAuthorization` (`sigs/typed_<chain>.json` in the plan's scratch):
/// one under "USDC" on Unichain (130), as its USDC answers `name()`, and one under
/// "USD Coin" on Base. Each recovers to its payer only under the name its chain's entry
/// holds, an entry naming none being "USD Coin".
#[test]
fn a_3009_signature_is_checked_under_its_chains_own_usdc_name() {
    let base_chain = sample().evm_chains.get(ARBITRUM).unwrap();
    let swapic: EvmAddress = STAGING_SWAPIC.parse().unwrap();
    let usdc_named: types::config::DomainText = "USDC".parse().unwrap();
    let cases = [
        (
            ChainId::new(130),
            "0x078D782b760474a361dDA0AF3839290b0EF57AD6",
            Some(usdc_named),
            None,
            "75a64b5dd29fff01a93403d0b78c738c4b946890d5574e2d2776519744c9a23a5dd02cde0d15366d746d8010263001dfe06752e04de94399192e35e301258e841b",
        ),
        (
            ChainId::BASE,
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            None,
            Some(usdc_named),
            "7fb83d9fde4a262d3aee626308a4cd17facc4dfc671ef2207107cafd2e3a29076a97dd049086a73cad94d431c72288c7d33c6601551155c36c18e9fbac15fa1c1c",
        ),
    ];
    for (chain_id, usdc, signed_under, other, signature) in cases {
        let usdc: EvmAddress = usdc.parse().unwrap();
        let order = anvil_order(usdc);
        let id = swapic::order_id(&order, chain_id, swapic);
        let auth = GaslessAuth {
            door: swapic::DOOR_AUTH_3009,
            validAfter: U256::ZERO,
            validBefore: order.deadline,
            nonce: id.as_nonce(),
            signature: hex::decode(signature).unwrap().into(),
            ..Default::default()
        };
        let entry = |usdc_name| EvmChain {
            usdc,
            swapic,
            usdc_name,
            ..base_chain
        };
        assert_eq!(
            authorize(&order, &auth, id, chain_id, &entry(signed_under)),
            Ok(()),
            "{chain_id} under its own name"
        );
        assert_eq!(
            authorize(&order, &auth, id, chain_id, &entry(other)),
            Err(RelayError::ContractPayerUnsupported),
            "{chain_id} under the other name"
        );
    }
}

/// An entry naming no domain reads as Circle's usual "USD Coin" and "2", and one naming
/// its own reads as it.
#[test]
fn a_chains_usdc_domain_is_its_entrys_or_circles_usual() {
    let chain = sample().evm_chains.get(ARBITRUM).unwrap();
    assert_eq!(chain.usdc_name, None, "the sample's entry names none");
    let usual = chain.usdc_domain();
    assert_eq!(
        (usual.address, usual.name, usual.version),
        (chain.usdc, "USD Coin", "2")
    );
    let named = EvmChain {
        usdc_name: Some("USDC".parse().unwrap()),
        usdc_version: Some("3".parse().unwrap()),
        ..chain
    };
    let own = named.usdc_domain();
    assert_eq!((own.name, own.version), ("USDC", "3"));
}
