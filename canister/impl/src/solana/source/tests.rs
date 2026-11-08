//! A fee proof from a real burn's shape: the program's instruction, CCTP's event, the attested message.

use super::*;
use swapic_solana_wire::args::{Payload, TAG_SWAP_BURN};
use swapic_solana_wire::cctp::{DEPOSIT_FOR_BURN_EVENT, EVENT_CPI_TAG};
use swapic_solana_wire::consts::{shard_of, TMM};
use swapic_solana_wire::instructions as ixs;
use swapic_solana_wire::message::{compile_v0, wire_v0, LookupTable};

const FEE: u32 = 600_000;
const AMOUNT: u64 = 999_400_000;
const MAX_FEE: u64 = 13_000;
const R: [u8; 32] = [4; 32];

fn addresses() -> SwapicAddresses {
    SwapicAddresses::derive([1; 32], [2; 32], [3; 32])
}

fn args(float: bool, fee: u32) -> Vec<u8> {
    BurnArgs {
        tag: TAG_SWAP_BURN,
        fast: true,
        event_keypair: false,
        float,
        domain: 6,
        ref_source: RefSource::Direct(&R),
        deadline: 1_800_001_800,
        recipient: &[0x11; 20],
        payload: Payload::Swap { token_out: &[0x22; 20], min_out: &[1, 2, 3] },
        min_gas: 700_000,
        max_fee_deci_bps: 13,
        amount: 1,
        fee,
        jupiter: &[0xbb, 0x64, 0xfa, 0xcc, 0x31, 0xc4, 0xaf, 0x14],
    }
    .encode()
}

fn envelope_of(data: &[u8]) -> [u8; 320] {
    BurnArgs::decode(data).unwrap().envelope(&R).encode()
}

fn message(a: &SwapicAddresses, envelope: &[u8], amount: u64) -> Vec<u8> {
    use swapic_solana_wire::cctp::*;
    let mut m = vec![0u8; HOOK_AT];
    m[HEADER_VERSION].copy_from_slice(&1u32.to_be_bytes());
    m[SOURCE_DOMAIN].copy_from_slice(&5u32.to_be_bytes());
    m[DESTINATION_DOMAIN].copy_from_slice(&6u32.to_be_bytes());
    m[NONCE].copy_from_slice(&[0xee; 32]);
    m[SENDER].copy_from_slice(&TMM);
    m[RECIPIENT].copy_from_slice(&[0x28; 32]);
    m[DESTINATION_CALLER].copy_from_slice(&[0x40; 32]);
    m[BODY_VERSION].copy_from_slice(&1u32.to_be_bytes());
    m[BURN_TOKEN].copy_from_slice(&USDC_MINT);
    m[MINT_RECIPIENT].copy_from_slice(&[0x40; 32]);
    m[AMOUNT.start + 24..AMOUNT.end].copy_from_slice(&amount.to_be_bytes());
    m[MESSAGE_SENDER].copy_from_slice(&a.auth[shard_of(&R)].0);
    m[MAX_FEE.start + 24..MAX_FEE.end].copy_from_slice(&super::tests::MAX_FEE.to_be_bytes());
    m.extend_from_slice(envelope);
    m
}

fn event(a: &SwapicAddresses, envelope: &[u8], amount: u64) -> Vec<u8> {
    let mut e = Vec::new();
    e.extend_from_slice(&EVENT_CPI_TAG);
    e.extend_from_slice(&DEPOSIT_FOR_BURN_EVENT);
    e.extend_from_slice(&USDC_MINT);
    e.extend_from_slice(&amount.to_le_bytes());
    e.extend_from_slice(&a.auth[shard_of(&R)].0);
    e.extend_from_slice(&[0x40; 32]);
    e.extend_from_slice(&6u32.to_le_bytes());
    e.extend_from_slice(&[0x28; 32]);
    e.extend_from_slice(&[0x40; 32]);
    e.extend_from_slice(&MAX_FEE.to_le_bytes());
    e.extend_from_slice(&1000u32.to_le_bytes());
    e.extend_from_slice(&(envelope.len() as u32).to_le_bytes());
    e.extend_from_slice(envelope);
    e
}

/// A landed burn: `[limit, price, swap_burn]`, its inner instructions at position 2.
fn landed(a: &SwapicAddresses, data: Vec<u8>, inner_event: Vec<u8>) -> LandedTx {
    let event_key = a.event(&R);
    let head = ixs::burn_head(a, &[9; 32], shard_of(&R), 6, false, &event_key, false);
    let swap = ixs::swap_burn(a, head, &[], data);
    let table = LookupTable { address: [77; 32], entries: ixs::swapic_table_entries(a) };
    let instructions = [ixs::compute_unit_limit(200_000), ixs::compute_unit_price(1000), swap];
    let compiled = compile_v0(&[9; 32], &instructions, &[table], &[7; 32]).unwrap();
    LandedTx {
        failed: false,
        wire: wire_v0(&[[1; 64]], &compiled.encode()),
        loaded_writable: vec![],
        loaded_readonly: vec![],
        inner: vec![(2, vec![(5, vec![1, 2, 3]), (6, inner_event)])],
        block_time_s: Some(1_800_000_000),
    }
}

fn needed() -> TokenAmount {
    TokenAmount::from(500_000u64)
}

fn gasless() -> TokenAmount {
    TokenAmount::from(440_000u64)
}

#[test]
fn a_real_shaped_burn_proves_its_message_and_its_fee() {
    let a = addresses();
    let data = args(false, FEE);
    let envelope = envelope_of(&data);
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    let proved = proves(&tx, &message(&a, &envelope, AMOUNT), &a, needed(), gasless()).unwrap();
    assert_eq!(proved, Proved { r#ref: R, fee: FEE as u64, float: false });
}

#[test]
fn a_failed_transaction_proves_nothing() {
    let a = addresses();
    let data = args(false, FEE);
    let envelope = envelope_of(&data);
    let mut tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    tx.failed = true;
    assert!(proves(&tx, &message(&a, &envelope, AMOUNT), &a, needed(), gasless()).unwrap_err().contains("failed"));
}

#[test]
fn an_unpaid_or_underpaid_burn_is_refused() {
    let a = addresses();
    let data = args(false, 499_999);
    let envelope = envelope_of(&data);
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    assert!(proves(&tx, &message(&a, &envelope, AMOUNT), &a, needed(), gasless()).unwrap_err().contains("less"));
}

#[test]
fn a_co_signed_burn_also_pays_the_gasless_row() {
    let a = addresses();
    let data = args(true, FEE);
    let envelope = envelope_of(&data);
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    assert!(proves(&tx, &message(&a, &envelope, AMOUNT), &a, needed(), gasless()).unwrap_err().contains("less"));
}

#[test]
fn a_message_that_differs_from_the_burn_event_is_refused() {
    // attack L4: one paid transaction never stands as proof for another message with the same ref
    let a = addresses();
    let data = args(false, FEE);
    let envelope = envelope_of(&data);
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    let other = message(&a, &envelope, AMOUNT + 1);
    assert!(proves(&tx, &other, &a, needed(), gasless()).unwrap_err().contains("does not match"));
}

#[test]
fn a_burn_by_anyone_but_an_auth_is_refused() {
    let a = addresses();
    let data = args(false, FEE);
    let envelope = envelope_of(&data);
    let mut forged = event(&a, &envelope, AMOUNT);
    forged[56..88].copy_from_slice(&[0x99; 32]);
    let tx = landed(&a, data, forged);
    let mut attested = message(&a, &envelope, AMOUNT);
    attested[swapic_solana_wire::cctp::MESSAGE_SENDER].copy_from_slice(&[0x99; 32]);
    assert!(proves(&tx, &attested, &a, needed(), gasless()).unwrap_err().contains("does not match"));
}

#[test]
fn a_message_the_program_did_not_burn_is_refused() {
    let a = addresses();
    let data = args(false, FEE);
    let envelope = envelope_of(&data);
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    let mut other_envelope = envelope;
    other_envelope[140] ^= 1;
    assert!(proves(&tx, &message(&a, &other_envelope, AMOUNT), &a, needed(), gasless()).unwrap_err().contains("did not burn"));
}

#[test]
fn a_memo_burn_is_matched_by_the_keccak_of_its_memo() {
    let a = addresses();
    let memo = [0x5a; 180];
    let r = keccak256(&memo);
    let data = BurnArgs {
        tag: TAG_SWAP_BURN,
        fast: true,
        event_keypair: false,
        float: false,
        domain: 6,
        ref_source: RefSource::Memo(&memo),
        deadline: 1_800_001_800,
        recipient: &[0x11; 20],
        payload: Payload::Deliver,
        min_gas: 0,
        max_fee_deci_bps: 13,
        amount: 1,
        fee: FEE,
        jupiter: &[0xbb, 0x64, 0xfa, 0xcc, 0x31, 0xc4, 0xaf, 0x14],
    }
    .encode();
    let envelope = BurnArgs::decode(&data).unwrap().envelope(&r).encode();
    let tx = landed(&a, data, event(&a, &envelope, AMOUNT));
    assert_eq!(proves(&tx, &message(&a, &envelope, AMOUNT), &a, needed(), gasless()).unwrap().r#ref, r);
}

#[test]
fn a_source_key_is_the_same_for_a_signature_and_differs_between_them() {
    assert_eq!(source_key(&[1; 64]), source_key(&[1; 64]));
    assert_ne!(source_key(&[1; 64]), source_key(&[2; 64]));
}
