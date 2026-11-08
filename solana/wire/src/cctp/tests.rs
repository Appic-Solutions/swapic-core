use super::*;

extern crate alloc;
use alloc::string::ToString;
use alloc::vec::Vec;
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn samples() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("../../vectors/cctp_messages.json")).unwrap().as_array().unwrap().clone()
}

fn word_of(number: &str) -> [u8; 32] {
    let value: u128 = number.parse().unwrap();
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&value.to_be_bytes());
    w
}

#[test]
fn real_messages_read_as_iris_decoded_them() {
    let all = samples();
    assert!(all.len() >= 8);
    for v in &all {
        let bytes = hex(v["message"].as_str().unwrap());
        let message = Message::new(&bytes).unwrap();
        let d = &v["decoded"];
        assert_eq!(message.header_version(), 1, "{}", v["name"]);
        assert_eq!(message.source_domain().to_string(), d["sourceDomain"].as_str().unwrap());
        assert_eq!(message.destination_domain().to_string(), d["destinationDomain"].as_str().unwrap());
        assert_eq!(message.nonce().as_slice(), hex(d["nonce"].as_str().unwrap()));
        assert_eq!(message.body_version(), 1);
        let b = &v["body"];
        assert_eq!(message.amount_word(), &word_of(b["amount"].as_str().unwrap()));
        assert_eq!(message.fee_executed_word(), &word_of(b["feeExecuted"].as_str().unwrap()));
        assert_eq!(message.max_fee_word(), &word_of(b["maxFee"].as_str().unwrap()));
    }
}

#[test]
fn a_real_forwarder_message_carries_the_long_hook_form() {
    use crate::envelope::ForwarderHook;
    let v = samples().into_iter().find(|v| v["name"] == "evm_to_sol_forwarded_ata_flag_33").unwrap();
    let bytes = hex(v["message"].as_str().unwrap());
    let message = Message::new(&bytes).unwrap();
    assert_eq!(message.hook().len(), 65);
    assert!(matches!(ForwarderHook::parse(message.hook()), Some(ForwarderHook::CreateFor(_))));
}

#[test]
fn a_real_burn_event_matches_its_attested_message_field_by_field() {
    let mut seen = 0;
    for v in samples() {
        let Some(event) = v["deposit_for_burn_event"].as_str() else { continue };
        let data = hex(event);
        let parsed = DepositForBurnEvent::parse(&data).expect("a real event parses");
        let bytes = hex(v["message"].as_str().unwrap());
        let message = Message::new(&bytes).unwrap();
        assert_eq!(parsed.burn_token, message.burn_token());
        assert_eq!(parsed.mint_recipient, message.mint_recipient());
        assert_eq!(parsed.depositor, message.message_sender());
        assert_eq!(parsed.destination_caller, message.destination_caller());
        assert_eq!(parsed.destination_domain, message.destination_domain());
        assert_eq!(parsed.destination_token_messenger, message.recipient());
        assert_eq!(Some(parsed.amount), word_to_u64(message.amount_word()));
        assert_eq!(Some(parsed.max_fee), word_to_u64(message.max_fee_word()));
        assert_eq!(parsed.hook, message.hook());
        seen += 1;
    }
    assert!(seen >= 1, "no sample held a burn event");
}

#[test]
fn a_burn_event_of_another_kind_or_length_is_refused() {
    let v = samples().into_iter().find(|v| v["deposit_for_burn_event"].is_string()).unwrap();
    let mut data = hex(v["deposit_for_burn_event"].as_str().unwrap());
    assert!(DepositForBurnEvent::parse(&data).is_some());
    data.push(0);
    assert!(DepositForBurnEvent::parse(&data).is_none());
    data.pop();
    data[8] ^= 1;
    assert!(DepositForBurnEvent::parse(&data).is_none());
}

#[test]
fn burn_data_is_the_measured_420_bytes_in_circle_s_order() {
    let hook = [3u8; 320];
    let data = BurnData {
        amount: 10,
        domain: 6,
        mint_recipient: &[1; 32],
        destination_caller: &[2; 32],
        max_fee: 11,
        min_finality: FINALITY_FAST,
        hook: &hook,
    }
    .encode();
    assert_eq!(data.len(), 420);
    assert_eq!(&data[..8], &DEPOSIT_FOR_BURN_WITH_HOOK);
    assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 10);
    assert_eq!(u32::from_le_bytes(data[16..20].try_into().unwrap()), 6);
    assert_eq!(&data[20..52], &[1; 32]);
    assert_eq!(&data[52..84], &[2; 32]);
    assert_eq!(u64::from_le_bytes(data[84..92].try_into().unwrap()), 11);
    assert_eq!(u32::from_le_bytes(data[92..96].try_into().unwrap()), 1000);
    assert_eq!(u32::from_le_bytes(data[96..100].try_into().unwrap()), 320);
    assert_eq!(&data[100..], &hook);
}

#[test]
fn discriminators_are_anchor_s_hashes() {
    use sha2::{Digest, Sha256};
    let of = |name: &str| -> [u8; 8] { Sha256::digest(name.as_bytes())[..8].try_into().unwrap() };
    assert_eq!(of("global:deposit_for_burn_with_hook"), DEPOSIT_FOR_BURN_WITH_HOOK);
    assert_eq!(of("global:receive_message"), RECEIVE_MESSAGE);
    assert_eq!(of("global:reclaim_event_account"), RECLAIM_EVENT_ACCOUNT);
}

#[test]
fn max_fee_rounds_up() {
    assert_eq!(max_fee_of(1_000_000, 10), 100);
    assert_eq!(max_fee_of(1_000_001, 10), 101);
    assert_eq!(max_fee_of(0, 10), 0);
    assert_eq!(max_fee_of(u64::MAX, 65_535) > 0, true);
}

#[test]
fn receive_and_reclaim_data_put_each_vec_behind_its_length() {
    let mut out = [0u8; 8 + 4 + 3 + 4 + 2];
    write_receive_data(&mut out, &[1, 2, 3], &[4, 5]);
    assert_eq!(&out[..8], &RECEIVE_MESSAGE);
    assert_eq!(&out[8..12], &3u32.to_le_bytes());
    assert_eq!(&out[12..15], &[1, 2, 3]);
    assert_eq!(&out[15..19], &2u32.to_le_bytes());
    assert_eq!(&out[19..], &[4, 5]);

    write_reclaim_data(&mut out, &[1, 2, 3], &[4, 5]);
    assert_eq!(&out[..8], &RECLAIM_EVENT_ACCOUNT);
    assert_eq!(&out[12..15], &[1, 2, 3]);
}

#[test]
fn a_message_too_short_for_its_fixed_part_is_refused() {
    assert!(Message::new(&[0u8; 375]).is_err());
    assert!(Message::new(&[0u8; 376]).is_ok());
}

#[test]
fn a_word_above_64_bits_does_not_fit() {
    let mut word = [0u8; 32];
    word[31] = 5;
    assert_eq!(word_to_u64(&word), Some(5));
    word[23] = 1;
    assert_eq!(word_to_u64(&word), None);
}
