//! What the rollout kit asks of the wire crate, so a shell script never computes a Solana address itself.
//!
//!   kit addresses <program> <payer key> <admin key>    every Swapic address and the table's entries, as JSON
//!   kit domain <domain> <EVM usdc>                     CCTP's two accounts a domain must have before it is enabled
//!   kit sign <keypair file>                            stdin: a wire transaction, base64. stdout: signed, base64.
//!
//! `sign` is for the canaries Jeff runs with his own test wallet: the key file is read here and never printed.

use std::io::Read;
use swapic_solana_wire::base58::{decode_pubkey, encode};
use swapic_solana_wire::instructions::swapic_table_entries;
use swapic_solana_wire::pda::{self, SwapicAddresses};

fn key(text: &str) -> [u8; 32] {
    decode_pubkey(text).unwrap_or_else(|| panic!("not a Solana address: {text}"))
}

fn addresses(args: &[String]) {
    let a = SwapicAddresses::derive(key(&args[0]), key(&args[1]), key(&args[2]));
    let list = |items: Vec<[u8; 32]>| items.iter().map(|k| format!("\"{}\"", encode(k))).collect::<Vec<_>>().join(",");
    println!(
        "{{\"program\":\"{}\",\"payer\":\"{}\",\"admin\":\"{}\",\"config\":\"{}\",\"rent\":\"{}\",\"fee_account\":\"{}\",\"auth\":[{}],\"vault\":[{}],\"wsol\":[{}],\"table_entries\":[{}]}}",
        encode(&a.program),
        encode(&a.canister),
        encode(&a.admin),
        encode(&a.config.0),
        encode(&a.rent.0),
        encode(&a.fee_account),
        list(a.auth.iter().map(|(k, _)| *k).collect()),
        list(a.vault.to_vec()),
        list(a.wsol.to_vec()),
        list(swapic_table_entries(&a)),
    );
}

fn domain(args: &[String]) {
    let domain: u32 = args[0].parse().expect("a domain is a number");
    let hex = args[1].trim_start_matches("0x");
    assert_eq!(hex.len(), 40, "an EVM address");
    let mut word = [0u8; 32];
    for i in 0..20 {
        word[12 + i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex");
    }
    println!(
        "{{\"remote_token_messenger\":\"{}\",\"token_pair\":\"{}\"}}",
        encode(&pda::remote_token_messenger(domain)),
        encode(&pda::token_pair(domain, &word)),
    );
}

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn from_base64(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut bits, mut have) = (0u32, 0);
    for c in text.trim().bytes().filter(|c| *c != b'=') {
        let v = TABLE.iter().position(|t| *t == c).expect("not base64") as u32;
        bits = (bits << 6) | v;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    out
}

fn to_base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Fills this key's own signature slot, wherever the message lists it among its signers.
fn sign(args: &[String]) {
    use ed25519_dalek::{Signer, SigningKey};
    let text = std::fs::read_to_string(&args[0]).expect("the key file reads");
    let bytes: Vec<u8> = serde_json::from_str(&text).expect("a Solana key file: a list of 64 numbers");
    let secret: [u8; 32] = bytes[..32].try_into().expect("a Solana key file: a list of 64 numbers");
    let signer = SigningKey::from_bytes(&secret);
    let public = signer.verifying_key().to_bytes();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin reads");
    let mut wire = from_base64(&input);
    let slots = wire[0] as usize;
    assert!(slots < 128, "a compact count this tool reads");
    let message_at = 1 + slots * 64;
    let message = wire[message_at..].to_vec();
    // a v0 message: its marker, then the three counts, then the keys
    let keys_at = if message[0] & 0x80 != 0 { 4 } else { 3 };
    let signers = usize::from(message[if message[0] & 0x80 != 0 { 1 } else { 0 }]);
    assert!(usize::from(message[keys_at]) < 128, "a compact count this tool reads");
    let first = keys_at + 1;
    let at = (0..signers)
        .find(|i| message[first + i * 32..first + (i + 1) * 32] == public)
        .expect("this key is not among the message's signers");
    let signature = signer.sign(&message).to_bytes();
    wire[1 + at * 64..1 + (at + 1) * 64].copy_from_slice(&signature);
    println!("{}", to_base64(&wire));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("addresses") if args.len() == 4 => addresses(&args[1..]),
        Some("domain") if args.len() == 3 => domain(&args[1..]),
        Some("sign") if args.len() == 2 => sign(&args[1..]),
        _ => {
            eprintln!("usage: kit addresses <program> <payer> <admin> | kit domain <domain> <usdc> | kit sign <key file>");
            std::process::exit(2);
        }
    }
}
