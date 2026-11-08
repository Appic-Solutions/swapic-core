//! Generates the program's constants from one checked-in file, with every derived address worked
//! out by the same crate the canister and the quoter use (design 4.2).

use std::fmt::Write as _;
use swapic_solana_wire::base58;
use swapic_solana_wire::pda::SwapicAddresses;

fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (name, rest) = line.split_once('=')?;
            (name.trim() == key).then(|| rest.trim().trim_matches('"'))
        })
        .unwrap_or_else(|| panic!("the constants file names no `{key}`"))
}

fn pubkey(text: &str, key: &str) -> [u8; 32] {
    let raw = value(text, key);
    assert_ne!(raw, "UNSET", "`{key}` is UNSET: name a constants file through SWAPIC_SOLANA_CONSTS");
    base58::decode_pubkey(raw).unwrap_or_else(|| panic!("`{key}` is not a Solana address: {raw}"))
}

fn evm(text: &str, key: &str) -> [u8; 20] {
    let raw = value(text, key).trim_start_matches("0x");
    assert_eq!(raw.len(), 40, "`{key}` is not an EVM address");
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).unwrap_or_else(|_| panic!("`{key}` is not hex"));
    }
    out
}

fn bytes(array: &[u8]) -> String {
    format!("{array:?}")
}

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-env-changed=SWAPIC_SOLANA_CONSTS");
    let path = std::env::var("SWAPIC_SOLANA_CONSTS").unwrap_or_else(|_| format!("{dir}/../consts.toml"));
    println!("cargo:rerun-if-changed={path}");
    println!("cargo:rerun-if-changed=build.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));

    let a = SwapicAddresses::derive(pubkey(&text, "program_id"), pubkey(&text, "canister"), pubkey(&text, "admin"));
    let receiver = evm(&text, "cctp_receiver");
    let mut receiver_word = [0u8; 32];
    receiver_word[12..].copy_from_slice(&receiver);

    let mut out = String::new();
    let mut row = |name: &str, ty: &str, val: String| {
        writeln!(out, "pub const {name}: {ty} = {val};").unwrap();
    };
    let keys = |list: &[[u8; 32]]| format!("[{}]", list.iter().map(|k| bytes(k)).collect::<Vec<_>>().join(","));
    let auth: Vec<[u8; 32]> = a.auth.iter().map(|x| x.0).collect();
    let auth_bumps: Vec<u8> = a.auth.iter().map(|x| x.1).collect();
    row("PROGRAM_ID", "[u8; 32]", bytes(&a.program));
    row("CANISTER", "[u8; 32]", bytes(&a.canister));
    row("ADMIN", "[u8; 32]", bytes(&a.admin));
    row("CONFIG", "[u8; 32]", bytes(&a.config.0));
    row("CONFIG_BUMP", "u8", a.config.1.to_string());
    row("AUTH", "[[u8; 32]; 4]", keys(&auth));
    row("AUTH_BUMP", "[u8; 4]", bytes(&auth_bumps));
    row("VAULT", "[[u8; 32]; 4]", keys(&a.vault));
    row("WSOL", "[[u8; 32]; 4]", keys(&a.wsol));
    row("DENYLIST", "[[u8; 32]; 4]", keys(&a.denylist));
    row("RENT", "[u8; 32]", bytes(&a.rent.0));
    row("RENT_BUMP", "u8", a.rent.1.to_string());
    row("FEE_ACCOUNT", "[u8; 32]", bytes(&a.fee_account));
    row("CCTP_RECEIVER", "[u8; 32]", bytes(&receiver_word));
    row("SWAPIC_EVM", "[u8; 20]", bytes(&evm(&text, "swapic_evm")));
    row("RUNNER_EVM", "[u8; 20]", bytes(&evm(&text, "runner_evm")));
    let target = format!("{}/consts.rs", std::env::var("OUT_DIR").unwrap());
    std::fs::write(target, out).unwrap();
}
