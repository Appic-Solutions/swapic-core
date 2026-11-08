use super::*;

extern crate alloc;
use alloc::string::String;
use alloc::vec;
use serde_json::Value;

fn b64(s: &str) -> Vec<u8> {
    let table = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("not base64"),
        }
    };
    let mut out = Vec::with_capacity(s.len());
    for chunk in s.as_bytes().chunks(4) {
        let n = chunk.iter().filter(|c| **c != b'=').count();
        let mut v = 0u32;
        for c in chunk.iter().take(n) {
            v = v << 6 | table(*c);
        }
        v <<= 6 * (4 - n);
        out.extend_from_slice(&v.to_be_bytes()[1..n]);
    }
    out
}

fn wires() -> Vec<(String, Vec<u8>)> {
    serde_json::from_str::<Value>(include_str!("../../vectors/wires.json"))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (String::from(v["label"].as_str().unwrap()), b64(v["wire"].as_str().unwrap())))
        .collect()
}

#[test]
fn every_captured_v0_wire_parses_and_re_encodes_to_the_same_bytes() {
    let mut seen = 0;
    for (label, wire) in wires() {
        if wire[0] == VERSION_V1_BYTE {
            continue;
        }
        let signed = SignedV0::parse(&wire).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        let message = V0Message::parse(signed.message).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(message.encode(), signed.message, "{label}");
        assert_eq!(wire_v0(&signed.signatures, signed.message), wire, "{label}");
        assert_eq!(message.wire_size(), wire.len(), "{label}");
        seen += 1;
    }
    assert!(seen >= 10, "{seen}");
}

#[test]
fn every_captured_v1_wire_parses_and_re_encodes_to_the_same_bytes() {
    let mut seen = 0;
    for (label, wire) in wires() {
        if wire[0] != VERSION_V1_BYTE {
            continue;
        }
        let (message, signatures) = V1Message::parse_wire(&wire).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(wire_v1(&message.encode(), &signatures), wire, "{label}");
        assert_eq!(message.config.compute_unit_limit, 1_400_000, "{label}");
        assert_eq!(message.config.loaded_data_limit, 64 * 1024 * 1024, "{label}");
        seen += 1;
    }
    assert!(seen >= 5, "{seen}");
}

#[test]
fn a_v0_message_with_trailing_bytes_or_a_bad_version_is_refused() {
    let (_, wire) = wires().into_iter().find(|(_, w)| w[0] != VERSION_V1_BYTE).unwrap();
    let signed = SignedV0::parse(&wire).unwrap();
    let mut extra = signed.message.to_vec();
    extra.push(0);
    assert_eq!(V0Message::parse(&extra), Err(MessageError::TrailingBytes));
    let mut other = signed.message.to_vec();
    other[0] = 1;
    assert_eq!(V0Message::parse(&other), Err(MessageError::NotV0));
    assert_eq!(V0Message::parse(&signed.message[..50]), Err(MessageError::Truncated));
}

#[test]
fn a_program_index_of_the_fee_payer_or_a_loaded_key_is_refused() {
    let message = V0Message {
        header: Header { required_signatures: 1, readonly_signed: 0, readonly_unsigned: 1 },
        static_keys: vec![[1; 32], [2; 32]],
        blockhash: [0; 32],
        instructions: vec![CompiledInstruction { program_index: 0, accounts: vec![], data: vec![] }],
        lookups: vec![],
    };
    assert_eq!(V0Message::parse(&message.encode()), Err(MessageError::BadIndex));

    let mut loaded = message.clone();
    loaded.instructions[0].program_index = 2;
    loaded.lookups.push(TableLookup { table: [3; 32], writable: vec![0], readonly: vec![] });
    assert_eq!(V0Message::parse(&loaded.encode()), Err(MessageError::BadIndex));

    let mut outside = message.clone();
    outside.instructions[0] = CompiledInstruction { program_index: 1, accounts: vec![2], data: vec![] };
    assert_eq!(V0Message::parse(&outside.encode()), Err(MessageError::BadIndex));

    let mut duplicate = message;
    duplicate.static_keys = vec![[1; 32], [1; 32]];
    duplicate.instructions[0].program_index = 1;
    assert_eq!(V0Message::parse(&duplicate.encode()), Err(MessageError::DuplicateAccount));
}

fn program_call() -> Instruction {
    Instruction {
        program: [9; 32],
        accounts: vec![
            AccountMeta::signer([2; 32]),
            AccountMeta::writable([3; 32]),
            AccountMeta::readonly([4; 32]),
            AccountMeta::writable([5; 32]),
            AccountMeta::readonly([6; 32]),
        ],
        data: vec![1, 2, 3],
    }
}

#[test]
fn compile_orders_signers_first_and_loads_only_what_a_table_may_hold() {
    let table = LookupTable { address: [8; 32], entries: vec![[6; 32], [5; 32], [9; 32], [2; 32]] };
    let message = compile_v0(&[1; 32], &[program_call()], &[table], &[7; 32]).unwrap();
    // payer, the other signer (a table may not provide a signer), a writable, the program, a readonly:
    // the program stays static although a table holds it, because a table cannot name a program
    assert_eq!(message.static_keys, vec![[1; 32], [2; 32], [3; 32], [9; 32], [4; 32]]);
    assert_eq!(message.header, Header { required_signatures: 2, readonly_signed: 1, readonly_unsigned: 2 });
    assert_eq!(message.lookups.len(), 1);
    assert_eq!(message.lookups[0].writable, vec![1]);
    assert_eq!(message.lookups[0].readonly, vec![0]);
    let ix = &message.instructions[0];
    assert_eq!(ix.program_index, 3);
    // the writable entry follows the statics, the readonly entry follows it
    assert_eq!(ix.accounts, vec![1, 2, 4, 5, 6]);
    // parses back
    assert_eq!(V0Message::parse(&message.encode()).unwrap(), message);
    assert!(message.is_static_writable(0));
    assert!(!message.is_static_writable(1));
    assert!(message.is_static_writable(2));
    assert!(!message.is_static_writable(3));
    assert!(message.is_signer(1));
    assert!(!message.is_signer(2));
}

#[test]
fn compile_merges_the_flags_of_an_account_named_twice() {
    let ix = Instruction {
        program: [9; 32],
        accounts: vec![AccountMeta::readonly([3; 32]), AccountMeta::writable_signer([3; 32])],
        data: vec![],
    };
    let message = compile_v0(&[1; 32], &[ix], &[], &[7; 32]).unwrap();
    assert_eq!(message.header.required_signatures, 2);
    assert_eq!(message.header.readonly_signed, 0);
    assert_eq!(message.static_keys[1], [3; 32]);
    assert_eq!(message.instructions[0].accounts, vec![1, 1]);
}

#[test]
fn v1_is_the_captured_layout_and_sets_all_its_config() {
    let config = V1Config { priority_lamports: 1000, compute_unit_limit: 1_400_000, loaded_data_limit: 64 << 20 };
    let message = compile_v1(&[1; 32], &[program_call()], &[7; 32], config).unwrap();
    let bytes = message.encode();
    assert_eq!(bytes[0], 129);
    assert_eq!(&bytes[1..4], &[2, 1, 3]);
    assert_eq!(&bytes[4..8], &[0x0f, 0, 0, 0]);
    assert_eq!(&bytes[8..40], &[7; 32]);
    assert_eq!(bytes[40], 1);
    assert_eq!(bytes[41], message.addresses.len() as u8);
    let wire = wire_v1(&bytes, &[[1; 64], [2; 64]]);
    let (parsed, signatures) = V1Message::parse_wire(&wire).unwrap();
    assert_eq!(parsed, message);
    assert_eq!(signatures, vec![[1; 64], [2; 64]]);
    let mut extra = wire;
    extra.push(0);
    assert_eq!(V1Message::parse_wire(&extra), Err(MessageError::TrailingBytes));
}

#[test]
fn v1_refuses_more_accounts_than_a_transaction_may_lock() {
    let accounts: Vec<AccountMeta> = (0..70u8)
        .map(|i| {
            let mut key = [0u8; 32];
            key[0] = i + 100;
            AccountMeta::readonly(key)
        })
        .collect();
    let ix = Instruction { program: [9; 32], accounts, data: vec![] };
    let config = V1Config { priority_lamports: 0, compute_unit_limit: 1, loaded_data_limit: 1 };
    assert_eq!(compile_v1(&[1; 32], &[ix], &[7; 32], config), Err(MessageError::TooManyAccounts));
}

#[test]
fn short_vectors_are_canonical() {
    let mut out = Vec::new();
    push_short(&mut out, 300);
    assert_eq!(out, vec![0xac, 0x02]);
    let mut r = Reader { bytes: &out };
    assert_eq!(r.short(), Ok(300));
    // a non-canonical zero continuation is refused
    let mut r = Reader { bytes: &[0x80, 0x00] };
    assert_eq!(r.short(), Err(MessageError::Truncated));
}

#[test]
fn compile_uses_the_fewest_tables() {
    // the first table holds one of the keys, the second holds all three: one table is enough
    let ix = Instruction {
        program: [9; 32],
        accounts: vec![AccountMeta::readonly([3; 32]), AccountMeta::readonly([4; 32]), AccountMeta::readonly([5; 32])],
        data: vec![],
    };
    let small = LookupTable { address: [20; 32], entries: vec![[3; 32]] };
    let big = LookupTable { address: [21; 32], entries: vec![[5; 32], [4; 32], [3; 32]] };
    let message = compile_v0(&[1; 32], &[ix], &[small, big], &[7; 32]).unwrap();
    assert_eq!(message.lookups.len(), 1);
    assert_eq!(message.lookups[0].table, [21; 32]);
    assert_eq!(message.static_keys.len(), 2);
    assert_eq!(V0Message::parse(&message.encode()).unwrap(), message);
}
