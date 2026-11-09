//! Every attack of design 8b becomes a refusal here.

use super::*;
use crate::solana::config_for_tests as solana_config;
use swapic_solana_wire::args::{BurnArgs, Payload, RefSource, TAG_BURN, TAG_RELEASE};
use swapic_solana_wire::consts::*;
use swapic_solana_wire::instructions as ixs;
use swapic_solana_wire::message::{compile_v0, wire_v0, AccountMeta, Instruction, LookupTable};
use swapic_solana_wire::pda::SwapicAddresses;
use types::config::PerAction;

const NOW: u64 = 1_800_000_000;
const USER: [u8; 32] = [9; 32];
const RECIPIENT: [u8; 20] = [0x11; 20];
const USDC_ON_BASE: [u8; 20] = [0x83; 20];

fn addresses() -> SwapicAddresses {
    SwapicAddresses::derive([1; 32], [2; 32], [3; 32])
}

fn chain() -> EvmChain {
    let fee = TokenAmount::from(60_000u64);
    EvmChain {
        swapic: EvmAddress::new([0x41; 20]),
        runner: EvmAddress::new([0x42; 20]),
        cctp_receiver: EvmAddress::new([0x43; 20]),
        usdc: EvmAddress::new(USDC_ON_BASE),
        token_messenger: EvmAddress::new([0x44; 20]),
        message_transmitter: EvmAddress::new([0x45; 20]),
        gas_limit: PerAction::all(types::GasAmount::from(1_500_000_u64)),
        gas_budget: PerAction::all(types::Wei::from(1_u64)),
        min_network_fee: PerAction::all(fee),
        usdc_name: None,
        usdc_version: None,
        block_time: std::time::Duration::from_secs(2),
    }
}

/// The arguments of a good burn; a test changes one thing.
struct Good {
    fee: u32,
    amount: u64,
    deadline: u32,
    float: bool,
    recipient: [u8; 20],
    domain: u8,
    max_fee_deci_bps: u16,
    token_out: [u8; 20],
}

impl Good {
    fn new() -> Self {
        Self {
            fee: 600_000,
            amount: 50_000_000,
            deadline: (NOW + 1800) as u32,
            float: true,
            recipient: RECIPIENT,
            domain: 6,
            max_fee_deci_bps: 13,
            token_out: [0x22; 20],
        }
    }

    fn data(&self) -> Vec<u8> {
        BurnArgs {
            tag: TAG_BURN,
            fast: true,
            event_keypair: false,
            float: self.float,
            domain: self.domain,
            ref_source: RefSource::Direct(&[5; 32]),
            deadline: self.deadline,
            recipient: &self.recipient,
            payload: Payload::Swap {
                token_out: &self.token_out,
                min_out: &[1, 2, 3],
            },
            min_gas: 700_000,
            max_fee_deci_bps: self.max_fee_deci_bps,
            amount: self.amount,
            fee: self.fee,
            jupiter: &[],
        }
        .encode()
    }
}

fn burn_ix(a: &SwapicAddresses, data: Vec<u8>) -> Instruction {
    let event = a.event(&[5; 32]);
    let head = ixs::burn_head(a, &USER, shard_of(&[5; 32]), 6, true, &event, false);
    ixs::burn(a, head, &[10; 32], data)
}

fn wire_of(a: &SwapicAddresses, payer: &[u8; 32], instructions: &[Instruction]) -> Vec<u8> {
    let table = LookupTable {
        address: [77; 32],
        entries: ixs::swapic_table_entries(a),
    };
    let message = compile_v0(payer, instructions, &[table], &[7; 32]).unwrap();
    let signatures = vec![[1u8; 64]; message.header.required_signatures as usize];
    wire_v0(&signatures, &message.encode())
}

fn run(a: &SwapicAddresses, config: &SolanaConfig, wire: &[u8]) -> Result<Passed, String> {
    let chain = chain();
    let destination = |domain: u32| (domain == 6).then_some(chain);
    check(
        wire,
        &Context {
            a,
            config,
            now_s: NOW,
            destination,
            pool: Some((6, EvmAddress::new([0xcc; 20]))),
        },
    )
}

fn budget() -> [Instruction; 2] {
    [
        ixs::compute_unit_limit(200_000),
        ixs::compute_unit_price(1_000),
    ]
}

fn refused_with(good: Good, want: &str) {
    let a = addresses();
    let [limit, price] = budget();
    let wire = wire_of(&a, &a.canister, &[limit, price, burn_ix(&a, good.data())]);
    let why = run(&a, &solana_config(), &wire).unwrap_err();
    assert!(why.contains(want), "{why}");
}

#[test]
fn a_good_gasless_burn_passes_and_names_its_parties() {
    let a = addresses();
    let [limit, price] = budget();
    let wire = wire_of(
        &a,
        &a.canister,
        &[limit, price, burn_ix(&a, Good::new().data())],
    );
    let passed = run(&a, &solana_config(), &wire).unwrap();
    assert_eq!(passed.user, USER);
    assert_eq!(passed.r#ref, [5; 32]);
    assert_eq!(
        passed.parties,
        vec![Party::Solana(USER), Party::Evm(EvmAddress::new(RECIPIENT))]
    );
    assert_eq!(passed.signers, vec![USER]);
}

#[test]
fn a_system_transfer_from_the_canister_is_refused() {
    let a = addresses();
    let [limit, price] = budget();
    let drain = ixs::system_transfer(&a.canister, &[66; 32], 1_000_000);
    let wire = wire_of(
        &a,
        &a.canister,
        &[limit, price, burn_ix(&a, Good::new().data()), drain],
    );
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("does not allow"));
}

#[test]
fn an_account_opened_at_the_canisters_cost_is_refused() {
    let a = addresses();
    let [limit, price] = budget();
    let farm = ixs::create_ata_idempotent(&a.canister, &USER, &USDC_MINT, &TOKEN_PROGRAM);
    let wire = wire_of(
        &a,
        &a.canister,
        &[limit, price, farm, burn_ix(&a, Good::new().data())],
    );
    assert!(run(&a, &solana_config(), &wire).is_err());
}

#[test]
fn two_swapic_instructions_are_refused() {
    let a = addresses();
    let [limit, price] = budget();
    let wire = wire_of(
        &a,
        &a.canister,
        &[
            limit,
            price,
            burn_ix(&a, Good::new().data()),
            burn_ix(&a, Good::new().data()),
        ],
    );
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("two Swapic"));
}

#[test]
fn a_complete_release_or_set_config_is_never_co_signed() {
    let a = addresses();
    let [limit, price] = budget();
    let mut ix = burn_ix(&a, Good::new().data());
    ix.data[0] = TAG_RELEASE;
    let wire = wire_of(&a, &a.canister, &[limit, price, ix]);
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("arguments"));

    // set_config needs the admin key anyway, and this door refuses its shape first
    let [limit, price] = budget();
    let config = swapic_solana_wire::args::Config {
        paused: true,
        event_keypair_allowed: false,
        bump: 0,
        swap_domains: 0,
        ck_domains: 0,
        min_burn: 0,
        min_deadline_s: 1,
        max_deadline_s: 2,
    };
    let wire = wire_of(
        &a,
        &a.canister,
        &[limit, price, ixs::set_config(&a, &config)],
    );
    assert!(run(&a, &solana_config(), &wire).is_err());
}

#[test]
fn the_fee_payer_must_be_the_payer_key() {
    let a = addresses();
    let [limit, price] = budget();
    let wire = wire_of(&a, &USER, &[limit, price, burn_ix(&a, Good::new().data())]);
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("fee payer"));
}

#[test]
fn the_payer_key_named_outside_its_slot_is_refused() {
    let a = addresses();
    let [limit, price] = budget();
    let mut ix = burn_ix(&a, Good::new().data());
    ix.accounts.push(AccountMeta::readonly(a.canister));
    let wire = wire_of(&a, &a.canister, &[limit, price, ix]);
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("outside its slot"));
}

#[test]
fn a_burn_that_does_not_spend_the_float_is_refused() {
    let a = addresses();
    let mut good = Good::new();
    good.float = false;
    let [limit, price] = budget();
    let wire = wire_of(&a, &a.canister, &[limit, price, burn_ix(&a, good.data())]);
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("float"));
}

#[test]
fn an_under_priced_fee_is_refused() {
    // the gasless row plus the destination's arrival row
    let mut good = Good::new();
    good.fee = 499_999;
    refused_with(good, "below");
}

#[test]
fn a_deadline_at_the_edge_is_refused() {
    let mut good = Good::new();
    good.deadline = (NOW + 600 + 119) as u32;
    refused_with(good, "deadline");
    let mut good = Good::new();
    good.deadline = (NOW + 7_201) as u32;
    refused_with(good, "deadline");
}

#[test]
fn a_small_burn_is_refused() {
    let mut good = Good::new();
    good.amount = 9_999_999;
    refused_with(good, "at least");
}

#[test]
fn a_max_fee_above_the_cap_is_refused() {
    let mut good = Good::new();
    good.max_fee_deci_bps = 101;
    refused_with(good, "max fee");
}

#[test]
fn an_arrival_that_cannot_be_finished_is_refused() {
    let mut good = Good::new();
    good.recipient = [0x41; 20];
    refused_with(good, "Swapic's contracts");
    let mut good = Good::new();
    good.token_out = USDC_ON_BASE;
    refused_with(good, "arriving USDC");
    let mut good = Good::new();
    good.domain = 7;
    refused_with(good, "not a chain");
}

#[test]
fn a_dear_compute_budget_is_refused() {
    let a = addresses();
    let mut low = solana_config();
    low.max_priority_lamports = 27_999;
    for (limit, price, config, want) in [
        (1_400_001, 1_000, solana_config(), "compute limit"),
        (200_000, 20_001, solana_config(), "compute price"),
        (1_400_000, 20_000, low, "priority"),
    ] {
        let wire = wire_of(
            &a,
            &a.canister,
            &[
                ixs::compute_unit_limit(limit),
                ixs::compute_unit_price(price),
                burn_ix(&a, Good::new().data()),
            ],
        );
        let why = run(&a, &config, &wire).unwrap_err();
        assert!(why.contains(want), "{why}");
    }
}

#[test]
fn a_missing_budget_or_trailing_bytes_are_refused() {
    let a = addresses();
    let wire = wire_of(&a, &a.canister, &[burn_ix(&a, Good::new().data())]);
    assert!(run(&a, &solana_config(), &wire)
        .unwrap_err()
        .contains("compute"));
    let [limit, price] = budget();
    let mut wire = wire_of(
        &a,
        &a.canister,
        &[limit, price, burn_ix(&a, Good::new().data())],
    );
    wire.push(0);
    assert!(run(&a, &solana_config(), &wire).is_err());
}

#[test]
fn a_v1_transaction_is_refused() {
    let a = addresses();
    assert!(run(&a, &solana_config(), &[129, 1, 0, 0]).is_err());
}

#[test]
fn the_signature_is_checked_for_real() {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&[8; 32]);
    let signature = key.sign(b"a message").to_bytes();
    assert!(verify(
        key.verifying_key().as_bytes(),
        b"a message",
        &signature
    ));
    assert!(!verify(
        key.verifying_key().as_bytes(),
        b"another",
        &signature
    ));
    assert!(!verify(&[0xff; 32], b"a message", &signature));
}

#[test]
fn the_limits_key_on_the_signer_and_the_failure_budget_serves_only_known_signers() {
    let config = solana_config();
    assert!(within_limits(&config, (false, false, 0), (0, 0)).is_ok());
    assert!(within_limits(&config, (true, true, 0), (0, 0))
        .unwrap_err()
        .contains("landed and failed"));
    assert!(within_limits(&config, (false, false, 10), (0, 0)).is_err());
    assert!(within_limits(&config, (false, false, 0), (300, 0)).is_err());
    assert!(within_limits(&config, (false, false, 0), (0, 30))
        .unwrap_err()
        .contains("failure budget"));
    assert!(within_limits(&config, (false, true, 0), (0, 30)).is_ok());
}

#[test]
fn one_co_sign_per_ref() {
    let hash = [1u8; 32];
    assert_eq!(second_request((hash, 7, NOW), &hash, NOW + 5), Ok(Some(7)));
    assert!(second_request((hash, 7, NOW), &[2; 32], NOW + 89).is_err());
    assert_eq!(second_request((hash, 7, NOW), &[2; 32], NOW + 90), Ok(None));
}

/// A memo burn to `pool`: its terms, in a burn the canister would pay for.
fn memo_wire(a: &SwapicAddresses, memo: &[u8], domain: u8, recipient: [u8; 20]) -> Vec<u8> {
    let data = BurnArgs {
        tag: TAG_BURN,
        fast: true,
        event_keypair: false,
        float: true,
        domain,
        ref_source: RefSource::Memo(memo),
        deadline: (NOW + 1800) as u32,
        recipient: &recipient,
        payload: Payload::Deliver,
        min_gas: 0,
        max_fee_deci_bps: 13,
        amount: 50_000_000,
        fee: 600_000,
        jupiter: &[],
    }
    .encode();
    let [limit, price] = budget();
    wire_of(a, &a.canister, &[limit, price, burn_ix(a, data)])
}

fn swap_memo() -> Vec<u8> {
    types::pool::memo::encode(&types::pool::DepositTerms {
        kind: types::pool::DepositKind::Swap {
            recipient: types::pool::IcpAccount {
                owner: candid::Principal::from_slice(&[4; 29]),
                subaccount: None,
            },
            token_out: candid::Principal::from_slice(&[5; 29]),
            min_out: 1,
        },
        min_usdc: TokenAmount::from(1u64),
        premium: TokenAmount::ZERO,
        icp_fees: TokenAmount::ZERO,
        move_fee: TokenAmount::ZERO,
        salt: [6; 32],
    })
    .unwrap()
}

#[test]
fn a_memo_burn_goes_to_the_canister_on_the_pool_chain_and_nowhere_else() {
    let a = addresses();
    let memo = swap_memo();
    let pool = [0xcc; 20];
    let passed = run(&a, &solana_config(), &memo_wire(&a, &memo, 6, pool)).unwrap();
    assert_eq!(passed.r#ref, keccak256(&memo));

    // the canister's address on another chain: nothing credits the deposit there
    let chain = chain();
    let two_chains = |domain: u32| (domain == 6 || domain == 3).then_some(chain);
    let elsewhere = check(
        &memo_wire(&a, &memo, 3, pool),
        &Context {
            a: &a,
            config: &solana_config(),
            now_s: NOW,
            destination: two_chains,
            pool: Some((6, EvmAddress::new(pool))),
        },
    );
    assert!(elsewhere.unwrap_err().contains("the pool chain"));
    // on the pool chain, to anyone else
    let other = run(&a, &solana_config(), &memo_wire(&a, &memo, 6, RECIPIENT));
    assert!(other.unwrap_err().contains("the pool chain"));
}
