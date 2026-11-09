//! The Solana doors against pocket-ic's test threshold keys: real Ed25519 signatures over messages the
//! canister built, as the wire crate reads them back.

use crate::client::settlement::{get_config_full, set_config};
use crate::settlement_suite::init::{init_arg, install, upgrade};
use candid::{decode_one, encode_args, encode_one, CandidType, Nat, Principal};
use pocket_ic::{PocketIc, PocketIcBuilder};
use serde::de::DeserializeOwned;
use settlement_api::types::config::{ActionAmounts, ActionGas, EvmChainConfig};
use settlement_api::types::errors::GuardError;
use settlement_api::types::solana::{
    ProposeSolanaUpgradeArgs, SolanaAddresses, SolanaConfig, SolanaFeesConfig, SolanaLifetime,
    SolanaProgramConfigArgs, SolanaUpgradeView,
};
use settlement_api::types::tx::{ChainRef, SignedTx};
use swapic_solana_wire::args::{BurnArgs, Payload, RefSource, TAG_BURN};
use swapic_solana_wire::base58;
use swapic_solana_wire::consts::shard_of;
use swapic_solana_wire::instructions as ixs;
use swapic_solana_wire::message::{compile_v0, wire_v0, LookupTable, SignedV0, V1Message};
use swapic_solana_wire::pda::SwapicAddresses;

const BLOCKHASH: [u8; 32] = [0x11; 32];

fn lifetime() -> SolanaLifetime {
    SolanaLifetime {
        blockhash: base58::encode(&BLOCKHASH),
        last_valid_block_height: 123,
    }
}

fn config() -> SolanaConfig {
    SolanaConfig {
        program_id: base58::encode(&[1; 32]),
        sol_rpc: Principal::anonymous(),
        key_name: "key_1".to_string(),
        min_network_fee: SolanaFeesConfig {
            gasless_burn: Nat::from(1u32),
            complete_new_account: Nat::from(1u32),
            release_new_account: Nat::from(1u32),
            forward_relay: Nat::from(1u32),
            reclaim: Nat::from(1u32),
        },
        max_cu_limit: 1_400_000,
        max_cu_price: 20_000,
        max_priority_lamports: 28_000,
        loaded_data_limit: 64 << 20,
        rent_float_low: 1,
        rent_float_target: 2,
        max_complete_attempts: 3,
        max_release_attempts: 3,
        complete_margin_s: 60,
        cosign_per_signer_per_hour: 10,
        cosign_global_per_hour: 300,
        cosign_failures_per_hour: 30,
        gasless_min_usdc: 10_000_000,
        deadline_margin_s: 120,
        program_min_deadline_s: 600,
        program_min_burn: 1_000_000,
        max_fee_deci_bps: 100,
        max_forward_fee: 500_000,
        upgrade_delay_s: 172_800,
        enabled_gasless: true,
        enabled_arrivals: true,
        arrival_table: None,
        program_event_keypair_allowed: false,
        program_paused: false,
    }
}

fn update<T: DeserializeOwned + CandidType>(
    pic: &PocketIc,
    canister: Principal,
    sender: Principal,
    method: &str,
    args: Vec<u8>,
) -> T {
    let reply = pic
        .update_call(canister, sender, method, args)
        .unwrap_or_else(|e| panic!("{method}: {e:?}"));
    decode_one(&reply).unwrap_or_else(|e| panic!("{method}: {e:?}"))
}

fn query<T: DeserializeOwned + CandidType>(
    pic: &PocketIc,
    canister: Principal,
    method: &str,
    args: Vec<u8>,
) -> T {
    let reply = pic
        .query_call(canister, Principal::anonymous(), method, args)
        .unwrap_or_else(|e| panic!("{method}: {e:?}"));
    decode_one(&reply).unwrap_or_else(|e| panic!("{method}: {e:?}"))
}

/// (pic, canister, admin), on a network holding the test threshold keys, with the Solana line set.
fn setup() -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 100_000_000_000_000);
    install(&pic, canister, admin, &init_arg()).expect("the default arg installs");
    let mut full = get_config_full(&pic, canister, admin).expect("a controller reads the config");
    full.solana = Some(config());
    set_config(&pic, canister, admin, &full).expect("the Solana config is valid");
    (pic, canister, admin)
}

fn derive(pic: &PocketIc, canister: Principal, admin: Principal) -> SolanaAddresses {
    let derived: Result<SolanaAddresses, String> = update(
        pic,
        canister,
        admin,
        "derive_solana_keys",
        encode_args(()).unwrap(),
    );
    derived.expect("the keys derive")
}

fn verify(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(key) else {
        return false;
    };
    key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
        .is_ok()
}

fn signed_txs(pic: &PocketIc, canister: Principal, admin: Principal) -> Vec<SignedTx> {
    let reply = pic
        .query_call(
            canister,
            admin,
            "signed_txs",
            encode_args((0u64, 100u64)).unwrap(),
        )
        .unwrap();
    decode_one(&reply).unwrap()
}

/// A chain the arrival can be read against: the design C suite's row, its addresses its own.
fn evm_chain() -> EvmChainConfig {
    let usdc = "0x8335000000000000000000000000000000000000";
    let amounts = |value: u128| ActionAmounts {
        relay_cctp: Nat::from(value),
        relay_eco: Nat::from(value),
        relay_local: Nat::from(value),
        complete_swap: Nat::from(value),
        complete_ck: Nat::from(value),
        relay_deliver: Nat::from(value),
        release: Nat::from(value),
        pool_payout: None,
        pool_transfer: None,
        pool_approve: None,
        pool_move: None,
        pool_move_finish: None,
        pool_ck_deposit: None,
        eco_rescue: None,
        sweep: None,
    };
    EvmChainConfig {
        swapic: "0x4100000000000000000000000000000000000000".to_string(),
        runner: "0x4200000000000000000000000000000000000000".to_string(),
        cctp_receiver: "0x4300000000000000000000000000000000000000".to_string(),
        usdc: usdc.to_string(),
        token_messenger: "0x4400000000000000000000000000000000000000".to_string(),
        message_transmitter: "0x4500000000000000000000000000000000000000".to_string(),
        gas_limit: ActionGas {
            relay_cctp: 600_000,
            relay_eco: 600_000,
            relay_local: 900_000,
            complete_swap: 1_300_000,
            complete_ck: 600_000,
            relay_deliver: 300_000,
            release: 300_000,
            pool_payout: None,
            pool_transfer: None,
            pool_approve: None,
            pool_move: None,
            pool_move_finish: None,
            pool_ck_deposit: None,
            eco_rescue: None,
            sweep: None,
        },
        // a tenth of an ether: the gas rule holds at the fees pushed below
        gas_budget_wei: amounts(100_000_000_000_000_000),
        // the relay on Arbitrum and the completion on Base, inside the 166,275 charged
        min_network_fee: ActionAmounts {
            relay_cctp: Nat::from(100_000_u32),
            complete_swap: Nat::from(60_000_u32),
            ..amounts(0)
        },
        usdc_domain_name: None,
        usdc_domain_version: None,
        block_time_ms: 2_000,
    }
}

#[test]
fn the_two_keys_derive_once_and_differ() {
    let (pic, canister, admin) = setup();
    let none: Option<SolanaAddresses> =
        query(&pic, canister, "solana_addresses", encode_args(()).unwrap());
    assert_eq!(none, None);
    // a stranger cannot derive
    let refused: Result<SolanaAddresses, String> = update(
        &pic,
        canister,
        Principal::from_slice(&[9; 29]),
        "derive_solana_keys",
        encode_args(()).unwrap(),
    );
    assert!(refused.is_err());
    let first = derive(&pic, canister, admin);
    assert_ne!(first.payer, first.admin);
    assert_eq!(
        base58::decode_pubkey(&first.payer).map(|k| k.len()),
        Some(32)
    );
    assert_eq!(derive(&pic, canister, admin), first);
    let read: Option<SolanaAddresses> =
        query(&pic, canister, "solana_addresses", encode_args(()).unwrap());
    assert_eq!(read, Some(first.clone()));
    // they survive an upgrade: nothing is derived again
    upgrade(&pic, canister, admin).expect("the upgrade goes through");
    assert_eq!(derive(&pic, canister, admin), first);
}

#[test]
fn the_v1_proof_is_a_real_signature_over_what_the_canister_built() {
    let (pic, canister, admin) = setup();
    let keys = derive(&pic, canister, admin);
    let payer = base58::decode_pubkey(&keys.payer).unwrap();
    let id: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "prove_solana_send",
        encode_one(lifetime()).unwrap(),
    );
    let id = id.unwrap();
    let tx = signed_txs(&pic, canister, admin)
        .into_iter()
        .find(|tx| tx.id == id)
        .expect("the watcher sees it");
    assert_eq!(tx.chain, ChainRef::Solana);
    assert_eq!(
        tx.hash.len(),
        64,
        "a Solana transaction's hash is its signature"
    );
    let (message, signatures) = V1Message::parse_wire(&tx.raw).unwrap();
    assert_eq!(message.addresses[0], payer);
    assert_eq!(message.blockhash, BLOCKHASH);
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].to_vec(), tx.hash);
    assert!(verify(&payer, &message.encode(), &signatures[0]));
}

#[test]
fn set_config_is_signed_by_the_admin_key_with_the_payer_key_paying() {
    let (pic, canister, admin) = setup();
    let keys = derive(&pic, canister, admin);
    let (payer, admin_key) = (
        base58::decode_pubkey(&keys.payer).unwrap(),
        base58::decode_pubkey(&keys.admin).unwrap(),
    );
    let args = SolanaProgramConfigArgs {
        paused: true,
        event_keypair_allowed: false,
        swap_domains: 1 << 6,
        ck_domains: 1,
        min_burn: 1_000_000,
        min_deadline_s: 600,
        max_deadline_s: 7_200,
        lifetime: lifetime(),
    };
    let id: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "set_solana_program_config",
        encode_one(args).unwrap(),
    );
    let tx = signed_txs(&pic, canister, admin)
        .into_iter()
        .find(|tx| tx.id == id.clone().unwrap())
        .unwrap();
    let (message, signatures) = V1Message::parse_wire(&tx.raw).unwrap();
    assert_eq!(signatures.len(), 2);
    let bytes = message.encode();
    // the payer key pays the transaction; set_config names the admin key
    assert_eq!(message.addresses[0], payer);
    assert_eq!(message.addresses[1], admin_key);
    for (key, signature) in message.addresses[..2].iter().zip(&signatures) {
        assert!(verify(key, &bytes, signature));
    }
}

#[test]
fn an_upgrade_waits_out_its_delay_and_only_a_controller_proposes() {
    let (pic, canister, admin) = setup();
    derive(&pic, canister, admin);
    let args = ProposeSolanaUpgradeArgs {
        buffer: base58::encode(&[5; 32]),
        build_hash: vec![6; 32],
        source_commit: "abc".to_string(),
    };
    let stranger = Principal::from_slice(&[9; 29]);
    let refused: Result<u64, String> = update(
        &pic,
        canister,
        stranger,
        "propose_solana_upgrade",
        encode_one(args.clone()).unwrap(),
    );
    assert!(refused.is_err());
    let id: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "propose_solana_upgrade",
        encode_one(args.clone()).unwrap(),
    );
    let id = id.unwrap();
    let live: Option<SolanaUpgradeView> = query(
        &pic,
        canister,
        "solana_live_upgrade",
        encode_args(()).unwrap(),
    );
    let live = live.unwrap();
    assert_eq!(live.id, id);
    assert_eq!(live.not_before_s - live.proposed_at_s, 172_800);
    // one at a time
    let second: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "propose_solana_upgrade",
        encode_one(args).unwrap(),
    );
    assert!(second.is_err());
    // before its delay, execution is refused before any read of Solana
    let early: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "execute_solana_upgrade",
        encode_args((id, lifetime())).unwrap(),
    );
    assert!(early.unwrap_err().contains("delay"));
    // a cancel clears it
    let cancelled: Result<(), String> = update(
        &pic,
        canister,
        admin,
        "cancel_solana_upgrade",
        encode_one(id).unwrap(),
    );
    cancelled.unwrap();
    let none: Option<SolanaUpgradeView> = query(
        &pic,
        canister,
        "solana_live_upgrade",
        encode_args(()).unwrap(),
    );
    assert_eq!(none, None);
}

/// A gasless burn the user signed with a real key, on a chain the canister's config knows.
fn gasless_tx(
    a: &SwapicAddresses,
    user: &ed25519_dalek::SigningKey,
    deadline: u32,
    r: [u8; 32],
    fee: u32,
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let user_key = *user.verifying_key().as_bytes();
    let data = BurnArgs {
        tag: TAG_BURN,
        fast: true,
        event_keypair: false,
        float: true,
        domain: 6,
        ref_source: RefSource::Direct(&r),
        deadline,
        recipient: &[0x11; 20],
        payload: Payload::Swap {
            token_out: &[0x22; 20],
            min_out: &[1, 2, 3],
        },
        min_gas: 700_000,
        max_fee_deci_bps: 13,
        amount: 50_000_000,
        fee,
        jupiter: &[],
    }
    .encode();
    let event = a.event(&r);
    let head = ixs::burn_head(a, &user_key, shard_of(&r), 6, true, &event, false);
    let burn = ixs::burn(a, head, &[10; 32], data);
    let table = LookupTable {
        address: [77; 32],
        entries: ixs::swapic_table_entries(a),
    };
    let message = compile_v0(
        &a.canister,
        &[
            ixs::compute_unit_limit(200_000),
            ixs::compute_unit_price(1000),
            burn,
        ],
        &[table],
        &BLOCKHASH,
    )
    .unwrap();
    let bytes = message.encode();
    let signature = user.sign(&bytes).to_bytes();
    wire_v0(&[[0; 64], signature], &bytes)
}

#[test]
fn the_door_refuses_a_stranger_and_a_bad_signature_and_co_signs_a_real_burn_once() {
    let (pic, canister, admin) = setup();
    let watcher = crate::settlement_suite::init::watcher();
    let keys = derive(&pic, canister, admin);
    let payer = base58::decode_pubkey(&keys.payer).unwrap();
    let a = SwapicAddresses::derive([1; 32], payer, base58::decode_pubkey(&keys.admin).unwrap());

    // the destination's chain and domain, so the arrival can be read
    let mut full = get_config_full(&pic, canister, admin).expect("a controller reads the config");
    let chain = 8453u64;
    full.cctp_domains.insert(chain, 6);
    full.evm_chains.insert(chain, evm_chain());
    set_config(&pic, canister, admin, &full).unwrap();

    let now = pic.get_time().as_nanos_since_unix_epoch() / 1_000_000_000;
    let user = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let r = [5u8; 32];
    let tx = gasless_tx(&a, &user, (now + 1800) as u32, r, 2_000_000);

    let args = encode_args((tx.clone(), 123u64)).unwrap();
    let stranger: Result<u64, String> = update(
        &pic,
        canister,
        Principal::from_slice(&[9; 29]),
        "cosign_solana",
        args.clone(),
    );
    assert!(stranger.is_err());

    // another key's signature
    let mut forged = tx.clone();
    forged[66] ^= 1;
    let bad: Result<u64, String> = update(
        &pic,
        canister,
        watcher,
        "cosign_solana",
        encode_args((forged, 123u64)).unwrap(),
    );
    assert!(bad.unwrap_err().contains("does not verify"));

    let id: Result<u64, String> = update(&pic, canister, watcher, "cosign_solana", args.clone());
    let id = id.expect("a good burn is co-signed");
    let signed = signed_txs(&pic, canister, admin)
        .into_iter()
        .find(|tx| tx.id == id)
        .unwrap();
    let parsed = SignedV0::parse(&signed.raw).unwrap();
    assert_eq!(parsed.signatures.len(), 2);
    assert!(verify(&payer, parsed.message, &parsed.signatures[0]));
    assert!(verify(
        user.verifying_key().as_bytes(),
        parsed.message,
        &parsed.signatures[1]
    ));
    assert_eq!(parsed.signatures[0].to_vec(), signed.hash);

    // asked again: the same line, no second signature
    let again: Result<u64, String> = update(&pic, canister, watcher, "cosign_solana", args);
    assert_eq!(again.unwrap(), id);

    // another message for the same ref is refused for 90 s
    let other = gasless_tx(&a, &user, (now + 1801) as u32, r, 2_000_000);
    let refused: Result<u64, String> = update(
        &pic,
        canister,
        watcher,
        "cosign_solana",
        encode_args((other, 123u64)).unwrap(),
    );
    assert!(refused.unwrap_err().contains("90 s"));

    // an under-priced burn is refused, and the door says why
    let cheap = gasless_tx(&a, &user, (now + 1800) as u32, [6; 32], 1);
    let refused: Result<u64, String> = update(
        &pic,
        canister,
        watcher,
        "cosign_solana",
        encode_args((cheap, 123u64)).unwrap(),
    );
    assert!(refused.unwrap_err().contains("below"));
}

#[test]
fn nothing_is_signed_while_the_line_is_switched_off_or_the_canister_is_halted() {
    let (pic, canister, admin) = setup();
    let watcher = crate::settlement_suite::init::watcher();
    derive(&pic, canister, admin);
    let mut full = get_config_full(&pic, canister, admin).expect("a controller reads the config");
    let mut solana = config();
    solana.enabled_gasless = false;
    full.solana = Some(solana);
    set_config(&pic, canister, admin, &full).unwrap();
    let off: Result<u64, String> = update(
        &pic,
        canister,
        watcher,
        "cosign_solana",
        encode_args((vec![0u8; 10], 1u64)).unwrap(),
    );
    assert!(off.unwrap_err().contains("off"));
}

fn program_config(paused: bool, min_burn: u64) -> SolanaProgramConfigArgs {
    SolanaProgramConfigArgs {
        paused,
        event_keypair_allowed: true,
        swap_domains: 1 << 6,
        ck_domains: 1,
        min_burn,
        min_deadline_s: 900,
        max_deadline_s: 7_200,
        lifetime: lifetime(),
    }
}

#[test]
fn the_programs_settings_move_the_canisters_reading_of_them() {
    let (pic, canister, admin) = setup();
    derive(&pic, canister, admin);
    let signed: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "set_solana_program_config",
        encode_one(program_config(true, 2_000_000)).unwrap(),
    );
    signed.unwrap();
    let held = get_config_full(&pic, canister, admin)
        .unwrap()
        .solana
        .unwrap();
    assert!(held.program_paused);
    assert!(held.program_event_keypair_allowed);
    assert_eq!(held.program_min_burn, 2_000_000);
    assert_eq!(held.program_min_deadline_s, 900);

    // a paused program refuses what is co-signed: the canister signs none
    let paused: Result<u64, String> = update(
        &pic,
        canister,
        crate::settlement_suite::init::watcher(),
        "cosign_solana",
        encode_args((vec![0u8; 10], 1u64)).unwrap(),
    );
    assert!(paused.unwrap_err().contains("paused"));
}

#[test]
fn a_pause_signs_under_a_halt_and_nothing_else_does() {
    let (pic, canister, admin) = setup();
    derive(&pic, canister, admin);
    let halted: Result<(), GuardError> = update(
        &pic,
        canister,
        admin,
        "set_halted",
        encode_one(true).unwrap(),
    );
    halted.unwrap();
    let unpause: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "set_solana_program_config",
        encode_one(program_config(false, 1_000_000)).unwrap(),
    );
    assert!(unpause.unwrap_err().contains("Halted"));
    let pause: Result<u64, String> = update(
        &pic,
        canister,
        admin,
        "set_solana_program_config",
        encode_one(program_config(true, 1_000_000)).unwrap(),
    );
    let id = pause.expect("a pause signs under a halt");
    assert!(signed_txs(&pic, canister, admin)
        .iter()
        .any(|tx| tx.id == id));
}

#[test]
fn the_key_name_and_the_section_stay_once_the_keys_are_derived() {
    let (pic, canister, admin) = setup();
    derive(&pic, canister, admin);
    let mut full = get_config_full(&pic, canister, admin).unwrap();
    let held = full.solana.clone();
    full.solana = None;
    assert!(set_config(&pic, canister, admin, &full).is_err());
    let mut renamed = held.clone().unwrap();
    renamed.key_name = "dfx_test_key".to_string();
    full.solana = Some(renamed);
    assert!(set_config(&pic, canister, admin, &full).is_err());
    full.solana = held;
    assert!(set_config(&pic, canister, admin, &full).is_ok());
}
