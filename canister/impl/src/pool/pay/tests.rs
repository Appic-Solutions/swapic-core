//! Review Focus 1 and 2 on REAL orders, the staging canaries' own (2026-10-18/19, mainnet,
//! Swapic `0xF0Ba68cB...`), copied into `samples/`; each test rewrites only the fields it
//! names, and says so:
//! - `same_chain_base.hex`: 1 USDC to ether on Base, user pays: one KyberSwap step, the
//!   Runner pays the ether out (the "token on Base" payout shape).
//! - `eco_base_to_arbitrum.hex`: 5 USDC on Base to USDC on Arbitrum through Eco, user pays:
//!   one `publishAndFund` delivering USDC (the "another chain" payout shape).
//! - `eco_with_fee_step.hex`: the same, gasless: a fee step to the canister, then the intent.
//! - `cctp_base_to_arbitrum.hex`: the same over CCTP fast: a fee step, then a burn.
//!
//! Every one is rewritten to be paid by the canister (`payer`, and an Eco reward's creator),
//! as a payout order is.

use super::*;
use crate::relay::Party;
use alloy_sol_types::SolCall;
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, GasAction, PerAction};
use types::pool::payout::{EvmWithdrawal, PayoutKind, PayoutTerms, Undo};
use types::pool::{PoolAccount, Shares};
use types::swapic::{publishAndFundCall, NATIVE};
use types::{CctpDomain, ChainId, TokenAmount};

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

fn swapic_address() -> EvmAddress {
    address("0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406")
}

/// The staging deploy: one Swapic, Runner and CctpReceiver address on every chain.
fn chain(usdc: EvmAddress) -> EvmChain {
    EvmChain {
        swapic: swapic_address(),
        runner: address("0x7E345acaB79102ac9588C60B9358Ec4d79438eEe"),
        cctp_receiver: address("0x23718C0a081B8bf00aee86722Af40E175562Ad47"),
        usdc,
        token_messenger: address("0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d"),
        message_transmitter: address("0x81D40F21F12A8F0E3252Bccb954D722d4c464B64"),
        gas_limit: PerAction::all(GasAmount::from(2_000_000_u64)),
        gas_budget: PerAction::all(Wei::from(10_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
    }
}

struct World {
    evm_chains: ChainTable<EvmChain>,
    domains: ChainTable<CctpDomain>,
}

impl World {
    fn new() -> Self {
        Self {
            evm_chains: ChainTable(BTreeMap::from([
                (ChainId::BASE, chain(base_usdc())),
                (ChainId::ARBITRUM, chain(arbitrum_usdc())),
            ])),
            domains: ChainTable(BTreeMap::from([
                (ChainId::BASE, CctpDomain::new(6)),
                (ChainId::ARBITRUM, CctpDomain::new(3)),
            ])),
        }
    }

    fn payer(&self) -> Payer<'_> {
        Payer {
            chains: Chains {
                chain_id: ChainId::BASE,
                evm_chains: &self.evm_chains,
                domains: &self.domains,
                canister: canister(),
                eco_portal: Some(address("0xEC000769A73b70e16f361a442292500b3BCf4A85")),
            },
            swapic: swapic_address(),
            usdc: base_usdc(),
            eco_prover: Some(address("0xEC08fb4647f3f50d1162a578d481266687C60fc5")),
        }
    }
}

fn sample(hex_text: &str) -> Order {
    swapic::decode_order(&hex::decode(hex_text.trim()).unwrap()).unwrap()
}

/// A real order rewritten to be the canister's: its payer, and an Eco step's reward creator.
fn the_canisters(mut order: Order) -> Order {
    order.payer = swapic::addr(canister());
    for step in &mut order.steps {
        if swapic::selector_of(step) == Some(swapic::PUBLISH_AND_FUND) {
            let mut call = publishAndFundCall::abi_decode(&step.data).unwrap();
            call.reward.creator = swapic::addr(canister());
            step.data = call.abi_encode().into();
        }
    }
    order
}

fn same_chain() -> Order {
    the_canisters(sample(include_str!("samples/same_chain_base.hex")))
}

fn eco() -> Order {
    the_canisters(sample(include_str!("samples/eco_base_to_arbitrum.hex")))
}

fn amount_of(order: &Order) -> TokenAmount {
    TokenAmount::from_be_bytes(order.amountIn.to_be_bytes::<32>())
}

fn deadline_of(order: &Order) -> u64 {
    u64::try_from(order.deadline).unwrap()
}

/// The terms the order delivers on, read by the canister's own reader: what a user who asked
/// for exactly this payout would have signed, its deadline the order's.
fn terms_of(order: &Order, world: &World) -> PayoutTerms {
    let delivery = read_plan(order, &world.payer().chains)
        .unwrap()
        .delivery
        .unwrap();
    PayoutTerms {
        dst_chain: delivery.chain,
        recipient: delivery.recipient,
        token_out: delivery.token,
        min_out: delivery.min_out,
        deadline: Timestamp::from_secs(deadline_of(order)).unwrap(),
    }
}

/// A withdrawal's payout waiting for `order`, its amount the order's `amountIn`.
fn waiting(order: &Order, terms: PayoutTerms) -> EvmPayout {
    let staker = address("0x1111111111111111111111111111111111111111");
    let withdrawal = EvmWithdrawal {
        staker,
        usdc: TokenAmount::from(10_000_000_u64),
        max_shares: Shares::new(1),
        move_fee: TokenAmount::from(1_000_u64),
        network_fee: TokenAmount::from(50_000_u64),
        nonce: 1,
        burned: Shares::new(1),
    };
    let mut payout = EvmPayout::new(
        3,
        PayoutKind::Withdrawal(withdrawal),
        terms,
        Undo::Remint {
            owner: PoolAccount::Evm(staker),
            shares: Shares::new(1),
        },
        Timestamp::from_nanos(1),
    );
    payout.amount = Some(amount_of(order));
    payout.status = PayoutStatus::Waiting;
    payout.base_reserved = true;
    payout
}

fn check(
    payout: &EvmPayout,
    order: &Order,
    world: &World,
    gas_limit: u64,
    sanctioned: impl Fn(&Party) -> bool,
) -> Result<PinnedOrder, PoolError> {
    check_with(
        payout,
        order,
        world,
        gas_limit,
        sanctioned,
        &PayoutSettings::default(),
    )
}

fn check_with(
    payout: &EvmPayout,
    order: &Order,
    world: &World,
    gas_limit: u64,
    sanctioned: impl Fn(&Party) -> bool,
    settings: &PayoutSettings,
) -> Result<PinnedOrder, PoolError> {
    check_order(
        payout,
        order,
        &swapic::encode_order(order),
        gas_limit,
        &world.payer(),
        settings,
        deadline_of(order) - 600,
        sanctioned,
        AmountIn::Exact,
    )
}

fn nobody(_: &Party) -> bool {
    false
}

#[test]
fn a_same_chain_payout_order_that_meets_its_terms_is_pinned() {
    let world = World::new();
    let order = same_chain();
    let terms = terms_of(&order, &world);
    // the ether on Base, the Runner paying at least the order's own minimum
    assert_eq!(terms.dst_chain, ChainId::BASE);
    assert_eq!(terms.token_out, NATIVE);
    let pinned = check(&waiting(&order, terms), &order, &world, 900_000, nobody).unwrap();
    assert_eq!(
        pinned.order_id,
        swapic::order_id(&order, ChainId::BASE, swapic_address()).into_bytes()
    );
    assert_eq!(pinned.gas_limit, 900_000);
    assert_eq!(pinned.deadline_s, deadline_of(&order));
    assert_eq!(swapic::decode_order(&pinned.order), Some(order));
}

/// Review Focus 1: each hostile variant of a real order is refused, and the payout keeps
/// waiting (the check records nothing).
#[test]
fn a_hostile_order_is_refused() {
    let world = World::new();
    let order = same_chain();
    let terms = terms_of(&order, &world);
    let payout = waiting(&order, terms);
    let refused = |order: &Order| check(&payout, order, &world, 900_000, nobody).is_err();
    // rewritten: the payout's recipient
    let mut other = order.clone();
    other.payout.recipient = swapic::addr(address("0x2222222222222222222222222222222222222222"));
    assert!(refused(&other), "a different recipient");
    // rewritten: the payout's recipient, to the canister itself
    let mut own = order.clone();
    own.payout.recipient = swapic::addr(canister());
    assert!(refused(&own), "an order paying the canister");
    // rewritten: the payout's minimum
    let mut smaller = order.clone();
    smaller.payout.minOut -= U256::from(1);
    assert!(refused(&smaller), "a smaller minimum");
    // rewritten: the payout's token
    let mut token = order.clone();
    token.payout.token = swapic::addr(base_usdc());
    assert!(refused(&token), "another token");
    // rewritten: the amount in
    let mut larger = order.clone();
    larger.amountIn += U256::from(1);
    assert!(refused(&larger), "a larger amountIn");
    // rewritten: the payer
    let mut payer = order.clone();
    payer.payer = swapic::addr(address("0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"));
    assert!(refused(&payer), "a payer other than the canister");
    // rewritten: the token in
    let mut token_in = order.clone();
    token_in.tokenIn = swapic::addr(NATIVE);
    assert!(refused(&token_in), "a token in other than USDC");
    // the terms on another chain
    let elsewhere = waiting(
        &order,
        PayoutTerms {
            dst_chain: ChainId::ARBITRUM,
            ..terms
        },
    );
    assert!(check(&elsewhere, &order, &world, 900_000, nobody).is_err());
    // a sanctioned recipient
    let recipient = terms.recipient;
    assert!(matches!(
        check(&payout, &order, &world, 900_000, |party| *party
            == Party::Evm(recipient)),
        Err(PoolError::Sanctioned(_))
    ));
    // a gas limit above the cap, and one below any transaction's
    assert!(check(&payout, &order, &world, 2_000_001, nobody).is_err());
    assert!(check(&payout, &order, &world, 20_999, nobody).is_err());
    // an order's deadline past the payout's, and one too close
    let early = waiting(
        &order,
        PayoutTerms {
            deadline: Timestamp::from_secs(deadline_of(&order) - 1).unwrap(),
            ..terms
        },
    );
    assert!(check(&early, &order, &world, 900_000, nobody).is_err());
    assert!(check_order(
        &payout,
        &order,
        &swapic::encode_order(&order),
        900_000,
        &world.payer(),
        &PayoutSettings::default(),
        deadline_of(&order) - ORDER_MARGIN_S + 1,
        nobody,
        AmountIn::Exact,
    )
    .is_err());
    // and the real one passes
    assert!(!refused(&order));
}

/// The "USDC on Base" shape: no step, the Runner pays the whole amount out.
#[test]
fn a_usdc_payout_on_base_with_no_step_is_read() {
    let world = World::new();
    let mut order = same_chain();
    // rewritten: no step, the payout in USDC, at least all of it
    order.steps.clear();
    order.payout.token = swapic::addr(base_usdc());
    order.payout.minOut = order.amountIn;
    let terms = terms_of(&order, &world);
    assert_eq!(terms.token_out, base_usdc());
    assert_eq!(terms.min_out, amount_of(&order));
    assert!(check(&waiting(&order, terms), &order, &world, 300_000, nobody).is_ok());
}

/// The "another chain" shape: one Eco intent, the canister its reward's creator (an
/// unfilled intent refunds the canister), USDC delivered on Arbitrum at least the step's
/// least delta less Eco's fee.
#[test]
fn an_eco_payout_order_is_read_and_its_refund_comes_back_to_the_canister() {
    let world = World::new();
    let order = eco();
    let terms = terms_of(&order, &world);
    assert_eq!(terms.dst_chain, ChainId::ARBITRUM);
    assert_eq!(terms.token_out, arbitrum_usdc());
    let payout = waiting(&order, terms);
    assert!(check(&payout, &order, &world, 600_000, nobody).is_ok());
    // the real order as the user signed it: its reward refunds the user, not the canister
    let mut theirs = sample(include_str!("samples/eco_base_to_arbitrum.hex"));
    theirs.payer = swapic::addr(canister());
    assert!(check(&payout, &theirs, &world, 600_000, nobody).is_err());
    // a smaller minimum asked than delivered is fine; a larger one is refused
    let greedy = waiting(
        &order,
        PayoutTerms {
            min_out: terms.min_out.checked_add(TokenAmount::ONE).unwrap(),
            ..terms
        },
    );
    assert!(check(&greedy, &order, &world, 600_000, nobody).is_err());
}

/// An Eco payout order's intent is held to Eco's own prover: the same order is refused
/// while the canister knows another prover, or none.
#[test]
fn an_eco_payout_order_is_refused_under_another_prover() {
    let world = World::new();
    let order = eco();
    let payout = waiting(&order, terms_of(&order, &world));
    let under = |eco_prover| {
        check_order(
            &payout,
            &order,
            &swapic::encode_order(&order),
            600_000,
            &Payer {
                eco_prover,
                ..world.payer()
            },
            &PayoutSettings::default(),
            deadline_of(&order) - 600,
            nobody,
            AmountIn::Exact,
        )
    };
    assert!(under(world.payer().eco_prover).is_ok());
    for prover in [
        Some(address("0x1111111111111111111111111111111111111111")),
        None,
    ] {
        assert!(matches!(
            under(prover),
            Err(PoolError::NotAllowed(why)) if why.contains("prover")
        ));
    }
}

/// A payout order WITH a fee step paying the canister is refused: the canister pays itself
/// nothing, or the fee would be booked as income and the books would drift.
#[test]
fn a_payout_order_paying_the_canister_a_fee_is_refused() {
    let world = World::new();
    let order = the_canisters(sample(include_str!("samples/eco_with_fee_step.hex")));
    let plan = read_plan(&order, &world.payer().chains).unwrap();
    assert_eq!(plan.fees.len(), 1);
    let terms = terms_of(&order, &world);
    assert!(matches!(
        check(&waiting(&order, terms), &order, &world, 600_000, nobody),
        Err(PoolError::NotAllowed(why)) if why.contains("no fee step")
    ));
}

/// A CCTP burn passes (the canister finishes its own transfer, `cctp::own`) while CCTP
/// payouts are on and the payout's network fee covers the destination's floor: the pool
/// chain's least fee for a burning order and the destination's for the finish. A Deliver
/// pays its recipient the destination's USDC, at least what the Runner gives the burn less
/// the most Circle may keep.
#[test]
fn a_cctp_payout_order_passes_the_checks() {
    let world = World::new();
    // the withdrawal's network fee in `waiting` is 50,000: exactly the least taken
    let settings = PayoutSettings {
        cctp_payouts: true,
        ..PayoutSettings::default()
    };
    let check = |payout: &EvmPayout, order: &Order, world: &World, gas, sanctioned| {
        check_with(payout, order, world, gas, sanctioned, &settings)
    };
    let mut order = the_canisters(sample(include_str!("samples/cctp_base_to_arbitrum.hex")));
    // rewritten: its fee step dropped, as a payout order pays the canister none
    order.steps.remove(0);
    let plan = read_plan(&order, &world.payer().chains).unwrap();
    let delivery = plan.delivery.unwrap();
    assert_eq!(delivery.chain, ChainId::ARBITRUM);
    assert_eq!(delivery.token, arbitrum_usdc());
    let burn = &order.steps[0];
    let call = types::swapic::depositForBurnWithHookCall::abi_decode(&burn.data).unwrap();
    assert_eq!(
        delivery.min_out,
        TokenAmount::from_be_bytes((burn.min - call.maxFee).to_be_bytes::<32>())
    );
    let terms = terms_of(&order, &world);
    let payout = waiting(&order, terms);
    assert!(check(&payout, &order, &world, 600_000, nobody).is_ok());
    // a minimum the burn cannot guarantee is refused
    let greedy = PayoutTerms {
        min_out: delivery.min_out.checked_add(TokenAmount::ONE).unwrap(),
        ..terms
    };
    assert!(matches!(
        check(&waiting(&order, greedy), &order, &world, 600_000, nobody),
        Err(PoolError::NotAllowed(why)) if why.contains("guarantees")
    ));
    // CCTP payouts off, or a network fee below the floor: the canister does not pay the
    // finish
    assert!(matches!(
        check_with(&payout, &order, &world, 600_000, nobody, &PayoutSettings::default()),
        Err(PoolError::NotAllowed(why)) if why.contains("off")
    ));
    let mut dearer = World::new();
    for (chain, action, fee) in [
        (ChainId::BASE, GasAction::RelayCctp, 30_000_u64),
        (ChainId::ARBITRUM, GasAction::RelayDeliver, 20_001),
    ] {
        let mut entry = dearer.evm_chains.get(chain).unwrap();
        match action {
            GasAction::RelayCctp => entry.min_network_fee.relay_cctp = TokenAmount::from(fee),
            _ => entry.min_network_fee.relay_deliver = TokenAmount::from(fee),
        }
        dearer.evm_chains.0.insert(chain, entry);
    }
    assert!(matches!(
        check(&payout, &order, &dearer, 600_000, nobody),
        Err(PoolError::NotAllowed(why)) if why.contains("at least 50001")
    ));
}

/// Item 6 of the final review fixes: a payout that pinned an order once is signed with no
/// order again, its own included.
#[test]
fn a_payout_that_pinned_an_order_signs_none_again() {
    let world = World::new();
    let order = same_chain();
    let terms = terms_of(&order, &world);
    let mut payout = waiting(&order, terms);
    let pinned = check(&payout, &order, &world, 900_000, nobody).unwrap();
    payout.order = Some(pinned);
    let mut fresh = order.clone();
    fresh.r#ref = alloy_primitives::FixedBytes([0x99; 32]);
    for order in [order, fresh] {
        assert!(matches!(
            check(&payout, &order, &world, 900_000, nobody),
            Err(PoolError::NotAllowed(why)) if why.contains("signed once")
        ));
    }
}

/// Review Focus 2 in the fold: two signatures racing for one payout, a call after it signed,
/// and a revert that is never signed again.
#[test]
fn one_payout_is_signed_once_at_a_time_and_never_again() {
    use crate::storage::events::append_event_at;
    use crate::storage::on_fresh_memory;
    use crate::storage::pool::tests::{at, base_stake, created, funded_pool, usdc};
    use crate::storage::pool::{payout as stored, record_at};
    use types::pool::PoolStep;
    on_fresh_memory(|| {
        crate::storage::init();
        funded_pool(usdc(1_000));
        record_at(
            PoolStep::PayoutSettingsSet {
                settings: PayoutSettings::default(),
            },
            at(1),
        )
        .unwrap();
        let staker = address("0x1111111111111111111111111111111111111111");
        base_stake(staker, usdc(500), 0x77, 1);
        let ledger = crate::storage::pool::ledger();
        let burned =
            Shares::burn_for(usdc(100), ledger.total_assets(), ledger.total_shares).unwrap();
        let withdrawal = EvmWithdrawal {
            staker,
            usdc: usdc(100),
            max_shares: burned,
            move_fee: TokenAmount::from(20_000_u64),
            network_fee: TokenAmount::from(50_000_u64),
            nonce: 1,
            burned,
        };
        let terms = PayoutTerms {
            dst_chain: ChainId::BASE,
            recipient: staker,
            token_out: base_usdc(),
            min_out: TokenAmount::ONE,
            deadline: at(1_000_000),
        };
        for step in [
            PoolStep::PayoutOpened {
                id: 0,
                kind: Box::new(PayoutKind::Withdrawal(withdrawal)),
                terms,
                undo: Undo::Remint {
                    owner: PoolAccount::Evm(staker),
                    shares: burned,
                },
                plan: None,
                ck: Default::default(),
            },
            PoolStep::PayoutAmountKnown {
                id: 0,
                amount: withdrawal.payout_amount().unwrap(),
                received: None,
            },
            PoolStep::PayoutWaiting { id: 0 },
        ] {
            record_at(step, at(2)).unwrap();
        }
        let order = |byte: u8| PinnedOrder {
            order_id: [byte; 32],
            order: vec![byte; 8],
            gas_limit: 300_000,
            deadline_s: 900,
        };
        let signed = |nonce: u64, byte: u8| PoolStep::PayoutSigned {
            id: 0,
            order: order(byte),
            chain_id: ChainId::BASE,
            nonce: types::Nonce::new(nonce),
            tx_hash: types::TxHash::new([byte; 32]),
            raw_tx: vec![byte; 4],
        };
        // the first call allocates; a second racing it is refused its own number
        append_event_at(created(TxPurpose::PoolPayout(0), 0), at(3)).unwrap();
        assert!(append_event_at(created(TxPurpose::PoolPayout(0), 1), at(3)).is_err());
        assert!(matches!(signable(0), Err(PoolError::Unavailable(_))));
        let index = record_at(signed(0, 1), at(4)).unwrap();
        // signed: a call after it answers the first transaction's id
        assert_eq!(signable(0), Ok(Some(index.get())));
        assert!(append_event_at(created(TxPurpose::PoolPayout(0), 1), at(5)).is_err());
        // reverted: its order stays pinned, and nothing is signed for it again
        record_at(PoolStep::PayoutReverted { id: 0, block: None }, at(6)).unwrap();
        assert_eq!(stored(0).unwrap().status, PayoutStatus::Reverted);
        assert_eq!(stored(0).unwrap().order, Some(order(1)));
        assert!(append_event_at(created(TxPurpose::PoolPayout(0), 1), at(7)).is_err());
        assert!(record_at(signed(1, 1), at(8)).is_err());
        assert!(matches!(signable(0), Err(PoolError::NotAllowed(_))));
        assert!(crate::storage::events::verify_replay());
    });
}

/// P11e (a HIGH of the collected list): a payout's onward intent names Eco's own prover; any
/// other could pay the reward, the canister's USDC, to whoever it says.
#[test]
fn a_payout_intent_of_another_prover_is_refused() {
    let world = World::new();
    let theirs = eco();
    let terms = terms_of(&theirs, &world);
    // rewritten: the reward's prover a stranger's
    let mut order = theirs.clone();
    let mut call = publishAndFundCall::abi_decode(&order.steps[0].data).unwrap();
    call.reward.prover = swapic::addr(address("0x000000000000000000000000000000000000dEaD"));
    order.steps[0].data = call.abi_encode().into();
    assert!(matches!(
        check(&waiting(&order, terms), &order, &world, 600_000, nobody),
        Err(PoolError::NotAllowed(why)) if why.contains("another prover")
    ));
    // with no prover configured, no Eco payout is signed
    let mut blind = world.payer();
    blind.eco_prover = None;
    assert!(matches!(
        check_order(
            &waiting(&theirs, terms),
            &theirs,
            &swapic::encode_order(&theirs),
            600_000,
            &blind,
            &PayoutSettings::default(),
            deadline_of(&theirs) - 600,
            nobody,
            AmountIn::Exact,
        ),
        Err(PoolError::NotAllowed(why)) if why.contains("no Eco prover")
    ));
}

/// One validator (2026-10-23): a planned swap's plan at its opening, its amount not known
/// yet, meets every rule a payout order meets once its amount is known, but the amount; so
/// an order refused for any other reason is refused alike in both modes.
#[test]
fn check_order_refuses_alike_whether_the_amount_is_known_or_not() {
    let world = World::new();
    let order = same_chain();
    let terms = terms_of(&order, &world);
    let known = waiting(&order, terms);
    let opening = EvmPayout {
        amount: None,
        status: PayoutStatus::Opened,
        base_reserved: false,
        ..known.clone()
    };
    let in_mode = |payout: &EvmPayout, order: &Order, gas: u64, mode: AmountIn| {
        check_order(
            payout,
            order,
            &swapic::encode_order(order),
            gas,
            &world.payer(),
            &PayoutSettings::default(),
            deadline_of(order) - 600,
            nobody,
            mode,
        )
    };
    let alike = |order: &Order, gas: u64, why: &str| {
        let exact = in_mode(&known, order, gas, AmountIn::Exact);
        let not_yet = in_mode(&opening, order, gas, AmountIn::NotKnownYet);
        assert!(exact.is_err(), "{why}");
        assert_eq!(exact, not_yet, "{why}");
    };
    let mut other = order.clone();
    other.payout.recipient = swapic::addr(address("0x2222222222222222222222222222222222222222"));
    alike(&other, 900_000, "another recipient");
    let mut smaller = order.clone();
    smaller.payout.minOut -= U256::from(1);
    alike(&smaller, 900_000, "a smaller minimum");
    let mut payer = order.clone();
    payer.payer = swapic::addr(address("0xf00EBA8E5Fd231b526ADFAAa68B4d0ede72A89AC"));
    alike(&payer, 900_000, "another payer");
    let mut token_in = order.clone();
    token_in.tokenIn = swapic::addr(NATIVE);
    alike(&token_in, 900_000, "another token in");
    alike(&order, 2_000_001, "a gas limit above the cap");
    alike(&order, 20_999, "a gas limit below any transaction's");
    // the amount alone: refused once known, not before
    let mut larger = order.clone();
    larger.amountIn += U256::from(1);
    assert!(in_mode(&known, &larger, 900_000, AmountIn::Exact).is_err());
    assert!(in_mode(&opening, &larger, 900_000, AmountIn::NotKnownYet).is_ok());
    // and the real order passes both
    assert!(in_mode(&known, &order, 900_000, AmountIn::Exact).is_ok());
    assert!(in_mode(&opening, &order, 900_000, AmountIn::NotKnownYet).is_ok());
    // a mode that does not match the payout is refused
    assert!(in_mode(&known, &order, 900_000, AmountIn::NotKnownYet).is_err());
    assert!(in_mode(&opening, &order, 900_000, AmountIn::Exact).is_err());
}

/// A payout the canister is fitting to its plan (its drive holds it) is not listed for the
/// watcher, which would quote for nothing; listed again once let go.
#[test]
fn payouts_waiting_leaves_out_a_payout_being_fitted() {
    use crate::storage::on_fresh_memory;
    use crate::storage::pool::tests::waiting_withdrawal;
    on_fresh_memory(|| {
        crate::storage::init();
        let id = waiting_withdrawal(address("0x1111111111111111111111111111111111111111"));
        assert_eq!(super::waiting().len(), 1);
        let held = crate::pool::payout::Busy::take(types::pool::evm_payout_ref(id).into_bytes());
        assert!(super::waiting().is_empty());
        drop(held);
        assert_eq!(super::waiting().len(), 1);
    });
}
