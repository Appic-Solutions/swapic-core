//! The local ICP demo's argument encoder (`scripts/demo-icp.sh`): candid arguments built
//! from the api crate's own types and printed as hex, so what dfx sends is exactly what the
//! canister decodes. Never prints a url: `init` reads them from a file and writes the
//! encoded argument to another.

use candid::{encode_args, encode_one, Nat, Principal};
use settlement_api::types::config::Config;
use settlement_api::types::flow::FlowSettings;
use settlement_api::types::init::InitArg;
use std::collections::BTreeMap;

fn arg(args: &[String], i: usize) -> &str {
    args.get(i).map(String::as_str).unwrap_or_else(|| {
        eprintln!("missing argument {i}");
        std::process::exit(2)
    })
}

fn principal(text: &str) -> Principal {
    Principal::from_text(text).expect("a principal")
}

fn bytes(hex_text: &str) -> Vec<u8> {
    hex::decode(hex_text.trim().trim_start_matches("0x")).expect("hex")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let encoded = match arg(&args, 1) {
        // init <admin principal> <file of chain=url lines> <out file>
        "init" => {
            let admin = principal(arg(&args, 2));
            let urls = std::fs::read_to_string(arg(&args, 3)).expect("the url file");
            let mut config = Config {
                confirmations: BTreeMap::from([(1, 12), (8453, 1), (42161, 1)]),
                chain_data_max_age_s: 86_400,
                rail_status_max_age_s: 3,
                batch_window_ms: 500,
                ecdsa_key_name: "dfx_test_key".to_string(),
                ..Config::default()
            };
            for line in urls.lines().filter(|line| !line.trim().is_empty()) {
                let (chain, url) = line.split_once('=').expect("chain=url");
                config
                    .rpc_urls
                    .insert(chain.parse().expect("a chain id"), url.to_string());
            }
            let init = InitArg {
                config,
                quoter: admin,
                watcher: admin,
            };
            let hex_text = hex::encode(encode_one(init).unwrap());
            std::fs::write(arg(&args, 4), hex_text).expect("the out file");
            return;
        }
        // flow-settings <unused> <unused> <unused> <eth usdc> <ckusdc> <minter> <pool>: the
        // three router arguments are kept in place and ignored, Swapic is in `evm_chains`
        "flow-settings" => encode_one(FlowSettings {
            ethereum_chain_id: 1,
            ethereum_usdc: Some(arg(&args, 5).to_string()),
            ckusdc_ledger: Some(principal(arg(&args, 6))),
            ck_minter: Some(principal(arg(&args, 7))),
            cketh_ledger: None,
            cketh_fee_allowance: Nat::from(0_u8),
            ck_helper: None,
            eco_prover: None,
            icpswap_pools: vec![principal(arg(&args, 8))],
            relay_window_s: 3_600,
            continuation_deadline_s: 1_800,
            max_gas_limit: 2_000_000,
            // the ICP flows cannot be turned on until they move to ABI v3 (P11)
            icp_enabled: None,
        }),
        // the icp-quote and evm-quote doors went with the ABI v2 ICP flows (P11a)
        // hash <hex>: one blob argument
        "hash" => encode_one(bytes(arg(&args, 2))),
        // relay <chain> <params hex> <auth hex>
        "relay" => encode_args((
            arg(&args, 2).parse::<u64>().unwrap(),
            bytes(arg(&args, 3)),
            bytes(arg(&args, 4)),
        )),
        other => {
            eprintln!("unknown command {other}");
            std::process::exit(2)
        }
    }
    .expect("candid encodes");
    println!("{}", hex::encode(encoded));
}
