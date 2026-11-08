//! Dumps the CCTP programs and state accounts the tests run against, from mainnet, read-only. Uses
//! the Solana CLI's `-um` moniker: no URL or key on any command line.

use std::process::Command;
use swapic_solana_wire::base58::encode;
use swapic_solana_wire::consts::*;
use swapic_solana_wire::pda;

fn word(evm: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..20 {
        out[12 + i] = u8::from_str_radix(&evm[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn run(args: &[&str]) {
    let status = Command::new("solana").args(args).status().expect("the Solana CLI");
    assert!(status.success(), "solana {args:?}");
}

fn main() {
    std::fs::create_dir_all("fixtures").unwrap();
    for (name, program) in [("message_transmitter_v2", MT), ("token_messenger_minter_v2", TMM)] {
        let path = format!("fixtures/{name}.so");
        if !std::path::Path::new(&path).exists() {
            run(&["-um", "program", "dump", &encode(&program), &path]);
        }
    }
    // Base and Ethereum: the sources the tests use
    let base_usdc = word("833589fcd6edb6e08f4c7c32d4f71b54bda02913");
    let eth_usdc = word("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    let accounts: Vec<[u8; 32]> = vec![
        MESSAGE_TRANSMITTER,
        TOKEN_MESSENGER,
        TOKEN_MINTER,
        LOCAL_TOKEN,
        USDC_MINT,
        CUSTODY,
        FEE_RECIPIENT_TOKEN_ACCOUNT,
        pda::remote_token_messenger(6),
        pda::remote_token_messenger(0),
        pda::token_pair(6, &base_usdc),
        pda::token_pair(0, &eth_usdc),
    ];
    for account in accounts {
        let text = encode(&account);
        let path = format!("fixtures/{text}.json");
        if !std::path::Path::new(&path).exists() {
            run(&["-um", "account", &text, "--output", "json-compact", "-o", &path]);
        }
    }
}
