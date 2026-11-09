//! The mainnet-fork run (design 7.1): real Jupiter routes through the real program and real CCTP on a
//! local Surfpool fork, with messages compiled by the wire crate. Answers the questions only real
//! state can: do the routes fit, do they land through the program, and does Jupiter close `wsol[i]`.

use base64::Engine;
use k256::ecdsa::SigningKey;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use swapic_solana_wire::args::{
    ArrivalArgs, BurnArgs, Config, Payload, RefSource, TAG_BURN, TAG_COMPLETE, TAG_RELEASE,
    TAG_SWAP_BURN,
};
use swapic_solana_wire::base58::{self, encode};
use swapic_solana_wire::consts::*;
use swapic_solana_wire::envelope::{SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_wire::instructions::{self as ixs, ArrivalSource};
use swapic_solana_wire::message::{
    compile_v0, compile_v1, wire_v0, wire_v1, AccountMeta, Instruction, LookupTable, V1Config,
    MAX_TX_ACCOUNTS, MAX_V0_TX_BYTES, MAX_V1_TX_BYTES,
};
use swapic_solana_wire::pda::{self, keccak256, SwapicAddresses};
use swapic_solana_wire::Pubkey;

const LOCAL: &str = "http://127.0.0.1:8899";
const JUPITER_API: &str = "https://api.jup.ag/swap/v2";
const BONK: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";
const JUP: &str = "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN";
const WIF: &str = "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm";
const SOL: &str = "So11111111111111111111111111111111111111112";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const PYUSD: &str = "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo";
/// Prop AMMs fail on stale oracles on a fork (design 7.1): classic AMMs only here.
const DEXES: &str = "Whirlpool,Raydium,Raydium CLMM,Raydium CP,Meteora DLMM,Meteora DAMM v2,Meteora,Orca V2,Phoenix";

fn key_of(label: &str) -> Pubkey {
    let seed: [u8; 32] = Sha256::digest(label.as_bytes()).into();
    let secret = solana_keypair::Keypair::new_from_array(seed);
    use solana_signer::Signer;
    secret.pubkey().to_bytes()
}

struct Fork {
    last_jupiter: std::time::Instant,
    attesters: Vec<SigningKey>,
    log: Vec<Value>,
}

#[derive(Debug, Default)]
struct Landed {
    err: Option<Value>,
    cu: u64,
    logs: Vec<String>,
}

fn sign_message_v0(message: &[u8], signers: usize) -> Vec<u8> {
    let mut signatures = vec![[0u8; 64]; signers];
    for s in &mut signatures {
        rand::thread_rng().fill_bytes(s);
    }
    wire_v0(&signatures, message)
}

impl Fork {
    fn call(&self, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let reply = ureq::post(LOCAL)
            .timeout(Duration::from_secs(120))
            .send_json(body);
        match reply {
            Ok(r) => r.into_json().expect("json"),
            Err(e) => panic!("{method}: {e}"),
        }
    }

    fn result(&self, method: &str, params: Value) -> Value {
        let v = self.call(method, params);
        if !v["error"].is_null() {
            panic!("{method}: {}", v["error"]);
        }
        v["result"].clone()
    }

    fn blockhash(&self) -> Pubkey {
        let v = self.result("getLatestBlockhash", json!([{"commitment":"confirmed"}]));
        base58::decode_pubkey(v["value"]["blockhash"].as_str().unwrap()).unwrap()
    }

    fn account(&self, key: &Pubkey) -> Option<(u64, Vec<u8>, Pubkey)> {
        let v = self.result(
            "getAccountInfo",
            json!([encode(key), {"encoding":"base64"}]),
        );
        if v["value"].is_null() {
            return None;
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(v["value"]["data"][0].as_str().unwrap())
            .unwrap();
        Some((
            v["value"]["lamports"].as_u64().unwrap(),
            data,
            base58::decode_pubkey(v["value"]["owner"].as_str().unwrap()).unwrap(),
        ))
    }

    fn lamports(&self, key: &Pubkey) -> u64 {
        self.account(key).map(|a| a.0).unwrap_or(0)
    }

    fn amount(&self, key: &Pubkey) -> u64 {
        let (_, data, _) = self.account(key).expect("a token account");
        u64::from_le_bytes(data[64..72].try_into().unwrap())
    }

    fn set_lamports(&self, key: &Pubkey, lamports: u64) {
        self.result(
            "surfnet_setAccount",
            json!([encode(key), {"lamports": lamports}]),
        );
    }

    fn now(&self) -> i64 {
        let (_, data, _) = self.account(&CLOCK_SYSVAR).expect("the clock");
        i64::from_le_bytes(data[32..40].try_into().unwrap())
    }

    fn slot(&self) -> u64 {
        self.result("getSlot", json!([{"commitment":"confirmed"}]))
            .as_u64()
            .unwrap()
    }

    fn send(&self, wire: &[u8]) -> Result<Landed, String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(wire);
        let v = self.call(
            "sendTransaction",
            json!([b64, {"encoding":"base64","skipPreflight":true,"maxRetries":0}]),
        );
        if !v["error"].is_null() {
            return Err(v["error"].to_string());
        }
        let signature = v["result"].as_str().unwrap().to_string();
        for _ in 0..80 {
            std::thread::sleep(Duration::from_millis(400));
            let t = self.result("getTransaction", json!([signature, {"encoding":"json","maxSupportedTransactionVersion":1,"commitment":"confirmed"}]));
            if !t.is_null() {
                let meta = &t["meta"];
                let logs = meta["logMessages"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|l| l.as_str().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let err = if meta["err"].is_null() {
                    None
                } else {
                    Some(meta["err"].clone())
                };
                return Ok(Landed {
                    err,
                    cu: meta["computeUnitsConsumed"].as_u64().unwrap_or(0),
                    logs,
                });
            }
        }
        Err("no status in time".to_string())
    }

    fn v1(&self, payer: &Pubkey, instructions: &[Instruction]) -> Vec<u8> {
        let config = V1Config {
            priority_lamports: 1000,
            compute_unit_limit: 1_400_000,
            loaded_data_limit: 64 * 1024 * 1024,
        };
        let message =
            compile_v1(payer, instructions, &self.blockhash(), config).expect("compiled v1");
        let mut signatures = vec![[0u8; 64]; message.header.required_signatures as usize];
        for s in &mut signatures {
            rand::thread_rng().fill_bytes(s);
        }
        wire_v1(&message.encode(), &signatures)
    }

    fn must_land(&self, label: &str, payer: &Pubkey, instructions: &[Instruction]) {
        let wire = self.v1(payer, instructions);
        match self.send(&wire) {
            Ok(Landed { err: None, .. }) => {}
            Ok(l) => panic!("{label}: {:?}\n{}", l.err, l.logs.join("\n")),
            Err(e) => panic!("{label}: {e}"),
        }
    }

    /// `/build` at most every 2.2 s: the keyless limit.
    fn build(&mut self, query: &[(&str, String)]) -> Result<Value, String> {
        let wait = Duration::from_millis(2200).saturating_sub(self.last_jupiter.elapsed());
        std::thread::sleep(wait);
        for attempt in 0..4 {
            self.last_jupiter = std::time::Instant::now();
            let mut request =
                ureq::get(&format!("{JUPITER_API}/build")).timeout(Duration::from_secs(30));
            for (k, v) in query {
                request = request.query(k, v);
            }
            match request.call() {
                Ok(r) => return r.into_json().map_err(|e| e.to_string()),
                Err(ureq::Error::Status(429, _)) => {
                    std::thread::sleep(Duration::from_secs(5 * (attempt + 1)))
                }
                Err(ureq::Error::Status(code, r)) => {
                    return Err(format!("{code}: {}", r.into_string().unwrap_or_default()))
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("429".to_string())
    }

    fn attest(&self, message: &[u8]) -> Vec<u8> {
        let hash = keccak256(message);
        let mut out = Vec::with_capacity(130);
        for key in &self.attesters {
            let (signature, recovery) = key.sign_prehash_recoverable(&hash).unwrap();
            out.extend_from_slice(&signature.to_bytes());
            out.push(27 + recovery.to_byte());
        }
        out
    }

    fn record(&mut self, row: Value) {
        println!("{row}");
        self.log.push(row);
    }
}

fn pubkey_text(v: &Value) -> Pubkey {
    base58::decode_pubkey(v.as_str().unwrap()).unwrap()
}

/// A Jupiter instruction as `/build` answers it. Route accounts never carry a signer flag out of
/// here: the program sets the one signer in its CPI.
fn route_of(build: &Value) -> (Vec<AccountMeta>, Vec<u8>) {
    let swap = &build["swapInstruction"];
    let accounts = swap["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| AccountMeta {
            pubkey: pubkey_text(&a["pubkey"]),
            signer: false,
            writable: a["isWritable"].as_bool().unwrap(),
        })
        .collect();
    let data = base64::engine::general_purpose::STANDARD
        .decode(swap["data"].as_str().unwrap())
        .unwrap();
    (accounts, data)
}

fn plain(ix: &Value) -> Instruction {
    Instruction {
        program: pubkey_text(&ix["programId"]),
        accounts: ix["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| AccountMeta {
                pubkey: pubkey_text(&a["pubkey"]),
                signer: a["isSigner"].as_bool().unwrap(),
                writable: a["isWritable"].as_bool().unwrap(),
            })
            .collect(),
        data: base64::engine::general_purpose::STANDARD
            .decode(ix["data"].as_str().unwrap())
            .unwrap(),
    }
}

fn tables_of(build: &Value) -> Vec<LookupTable> {
    build["addressesByLookupTableAddress"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(address, entries)| LookupTable {
                    address: base58::decode_pubkey(address).unwrap(),
                    entries: entries
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(pubkey_text)
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn labels_of(build: &Value) -> Vec<String> {
    build["routePlan"]
        .as_array()
        .map(|p| {
            p.iter()
                .map(|s| s["swapInfo"]["label"].as_str().unwrap_or("?").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn setup_of(build: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for (name, list) in [
        ("setup", build["setupInstructions"].clone()),
        ("cleanup", json!([build["cleanupInstruction"].clone()])),
        ("other", build["otherInstructions"].clone()),
    ] {
        for ix in list.as_array().cloned().unwrap_or_default() {
            if ix.is_null() {
                continue;
            }
            let accounts: Vec<String> = ix["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    format!(
                        "{}{}{}",
                        m["pubkey"].as_str().unwrap(),
                        if m["isSigner"].as_bool().unwrap() {
                            " s"
                        } else {
                            ""
                        },
                        if m["isWritable"].as_bool().unwrap() {
                            " w"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            out.push(json!({"kind": name, "program": ix["programId"], "accounts": accounts}));
        }
    }
    out
}

fn swapic_in_route(a: &SwapicAddresses, build: &Value) -> Vec<String> {
    let swap = &build["swapInstruction"];
    let mut hits = Vec::new();
    for (i, m) in swap["accounts"].as_array().unwrap().iter().enumerate() {
        let key = pubkey_text(&m["pubkey"]);
        let name = if key == a.canister {
            Some("canister")
        } else if key == a.admin {
            Some("admin")
        } else if key == a.config.0 {
            Some("config")
        } else if key == a.rent.0 {
            Some("rent")
        } else if a.auth_index(&key).is_some() {
            Some("auth")
        } else if a.vault_index(&key).is_some() {
            Some("vault")
        } else if a.wsol.contains(&key) {
            Some("wsol")
        } else {
            None
        };
        if let Some(name) = name {
            hits.push(format!(
                "{i}:{name}{}{}",
                if m["isSigner"].as_bool().unwrap() {
                    " s"
                } else {
                    ""
                },
                if m["isWritable"].as_bool().unwrap() {
                    " w"
                } else {
                    ""
                }
            ));
        }
    }
    hits
}

fn random_ref() -> [u8; 32] {
    let mut r = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut r);
    r
}

/// One burn out of Solana, composed as the quoter will: the fit loop steps the cap down.
struct OutCase<'a> {
    label: &'a str,
    mint: &'a str,
    amount: &'a str,
    gasless: bool,
    memo: Option<&'a [u8]>,
    ck_principal: Option<&'a [u8; 32]>,
}

fn run_out(
    f: &mut Fork,
    a: &SwapicAddresses,
    table: &LookupTable,
    user: &Pubkey,
    case: &OutCase<'_>,
) {
    let mut outcome = json!({"case": case.label, "fits": false});
    for cap in [48, 40, 32] {
        let mut r = random_ref();
        if let Some(memo) = case.memo {
            r = keccak256(memo);
        }
        let shard = shard_of(&r);
        let vault = a.vault[shard];
        let query = vec![
            ("inputMint", case.mint.to_string()),
            ("outputMint", USDC.to_string()),
            ("amount", case.amount.to_string()),
            ("taker", encode(user)),
            ("slippageBps", "200".to_string()),
            ("maxAccounts", cap.to_string()),
            ("destinationTokenAccount", encode(&vault)),
            ("dexes", DEXES.to_string()),
        ];
        let build = match f.build(&query) {
            Ok(b) => b,
            Err(e) => {
                outcome["build_error"] = json!(e);
                continue;
            }
        };
        let (mut route, jupiter) = route_of(&build);
        if jupiter[..8] == swapic_solana_wire::jupiter::ROUTE_V2 {
            ixs::route_straight_to(
                &mut route,
                &pda::ata(user, &USDC_MINT, &TOKEN_PROGRAM),
                &vault,
            );
            outcome["straight"] = json!(true);
        }
        let min_usdc: u64 = build["otherAmountThreshold"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let domain = if case.ck_principal.is_some() {
            0u8
        } else {
            6u8
        };
        let now = f.now();
        let token_out = [0x42u8; 20];
        let min_out = 1_000_000u64.to_be_bytes();
        let payload = match (case.memo, case.ck_principal) {
            (Some(_), _) => Payload::Deliver,
            (_, Some(principal)) => Payload::CkDeposit {
                principal,
                subaccount: None,
            },
            _ => Payload::Swap {
                token_out: &token_out,
                min_out: &min_out,
            },
        };
        let ref_source = match case.memo {
            Some(memo) => RefSource::Memo(memo),
            None => RefSource::Direct(&r),
        };
        let args = BurnArgs {
            tag: TAG_SWAP_BURN,
            fast: true,
            event_keypair: false,
            float: case.gasless,
            domain,
            ref_source,
            deadline: (now + 1800) as u32,
            recipient: &[0x11; 20],
            payload,
            min_gas: if case.memo.is_some() { 0 } else { 709_154 },
            max_fee_deci_bps: 13,
            amount: min_usdc,
            fee: 50_000,
            jupiter: &jupiter,
        };
        let event = a.event(&r);
        let head = ixs::burn_head(a, user, shard, domain as u32, case.gasless, &event, false);
        let swap = ixs::swap_burn(a, head, &route, args.encode());

        // the user's own wSOL setup and cleanup stay on a user-paid SOL swap; a gasless one has none
        let mut all = vec![
            ixs::compute_unit_limit(600_000),
            ixs::compute_unit_price(1000),
        ];
        let setups = build["setupInstructions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let kept: Vec<Instruction> = setups
            .iter()
            .map(plain)
            // nothing in USDC is opened: the route pays the vault itself
            .filter(|ix| {
                !(ix.program == ATA_PROGRAM
                    && ix
                        .accounts
                        .iter()
                        .any(|m| m.pubkey == vault || m.pubkey == USDC_MINT))
            })
            .collect();
        if case.gasless && !kept.is_empty() {
            outcome["gasless_refused_setup"] = json!(setup_of(&build));
            break;
        }
        all.extend(kept);
        all.push(swap);
        if !case.gasless && !build["cleanupInstruction"].is_null() {
            all.push(plain(&build["cleanupInstruction"]));
        }

        let payer = if case.gasless { a.canister } else { *user };
        // Swapic's table first: a table costs 32 bytes and more, so the fewer of Jupiter's that are left
        // with anything to give, the smaller the message
        let mut tables = vec![table.clone()];
        tables.extend(tables_of(&build));
        let message = match compile_v0(&payer, &all, &tables, &f.blockhash()) {
            Ok(m) => m,
            Err(e) => {
                outcome["compile_error"] = json!(format!("{e:?}"));
                continue;
            }
        };
        let (size, accounts) = (message.wire_size(), message.total_accounts());
        if size > MAX_V0_TX_BYTES || accounts > MAX_TX_ACCOUNTS {
            outcome[format!("cap{cap}")] = json!({"size": size, "accounts": accounts});
            continue;
        }
        let wire = sign_message_v0(
            &message.encode(),
            message.header.required_signatures as usize,
        );
        let vault_before = f.amount(&vault);
        let fee_before = f.amount(&a.fee_account);
        let landed = f.send(&wire);
        outcome["fits"] = json!(true);
        outcome["cap"] = json!(cap);
        outcome["size"] = json!(size);
        outcome["accounts"] = json!(accounts);
        outcome["route"] = json!(labels_of(&build));
        match landed {
            Ok(l) => {
                outcome["cu"] = json!(l.cu);
                outcome["own_cu"] = json!(swapic_solana_tests::own_compute(&l.logs, &a.program));
                outcome["err"] = json!(l.err);
                if l.err.is_none() {
                    assert_eq!(
                        f.amount(&vault),
                        vault_before,
                        "{}: the vault is not at its baseline",
                        case.label
                    );
                    assert_eq!(f.amount(&a.fee_account) - fee_before, 50_000);
                    let (_, event_data, _) = f.account(&event).expect("the event account");
                    assert_eq!(
                        event_data.len(),
                        748,
                        "{}: CCTP's event account",
                        case.label
                    );
                } else {
                    outcome["tail"] =
                        json!(l.logs.iter().rev().take(6).cloned().collect::<Vec<_>>());
                }
            }
            Err(e) => outcome["send_error"] = json!(e),
        }
        break;
    }
    f.record(outcome);
}

/// One arrival: a crafted message from Base with our test attesters, a real route for the minted USDC.
struct ArrivalCase<'a> {
    label: &'a str,
    mint: &'a str,
    token_program: Pubkey,
    native: bool,
}

const ARRIVAL_AMOUNT: u64 = 1_000_000_000;
const ARRIVAL_FEE: u64 = 130_000;

fn craft(f: &Fork, shard: usize, a: &SwapicAddresses, hook: &[u8]) -> ([u8; 32], Vec<u8>, Vec<u8>) {
    use swapic_solana_wire::cctp;
    let nonce = random_ref();
    let mut m = vec![0u8; cctp::HOOK_AT];
    let word = |evm: &[u8]| {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(evm);
        w
    };
    let number = |n: u64| {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&n.to_be_bytes());
        w
    };
    m[cctp::HEADER_VERSION].copy_from_slice(&1u32.to_be_bytes());
    m[cctp::SOURCE_DOMAIN].copy_from_slice(&6u32.to_be_bytes());
    m[cctp::DESTINATION_DOMAIN].copy_from_slice(&SOLANA_DOMAIN.to_be_bytes());
    m[cctp::NONCE].copy_from_slice(&nonce);
    m[cctp::SENDER].copy_from_slice(&word(&swapic_solana_tests::BASE_TOKEN_MESSENGER));
    m[cctp::RECIPIENT].copy_from_slice(&TMM);
    m[cctp::DESTINATION_CALLER].copy_from_slice(&a.auth[shard].0);
    m[140..144].copy_from_slice(&1000u32.to_be_bytes());
    m[144..148].copy_from_slice(&1000u32.to_be_bytes());
    m[cctp::BODY_VERSION].copy_from_slice(&1u32.to_be_bytes());
    m[cctp::BURN_TOKEN].copy_from_slice(&word(&swapic_solana_tests::BASE_USDC));
    m[cctp::MINT_RECIPIENT].copy_from_slice(&a.vault[shard]);
    m[cctp::AMOUNT].copy_from_slice(&number(ARRIVAL_AMOUNT));
    m[cctp::MESSAGE_SENDER].copy_from_slice(&word(&[0xab; 20]));
    m[cctp::MAX_FEE].copy_from_slice(&number(ARRIVAL_FEE));
    m[cctp::FEE_EXECUTED].copy_from_slice(&number(ARRIVAL_FEE));
    m.extend_from_slice(hook);
    let attestation = f.attest(&m);
    (nonce, m, attestation)
}

fn run_arrival(f: &mut Fork, a: &SwapicAddresses, wallet: &Pubkey, case: &ArrivalCase<'_>) {
    let minted = ARRIVAL_AMOUNT - ARRIVAL_FEE;
    let mut outcome = json!({"case": case.label, "fits": false});
    let mint = base58::decode_pubkey(case.mint).unwrap();
    let output = if case.native {
        *wallet
    } else {
        pda::ata(wallet, &mint, &case.token_program)
    };
    let source_token = {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(&swapic_solana_tests::BASE_USDC);
        w
    };
    for cap in [48, 40, 32] {
        let r = random_ref();
        let shard = shard_of(&r);
        let mut query = vec![
            ("inputMint", USDC.to_string()),
            ("outputMint", case.mint.to_string()),
            ("amount", minted.to_string()),
            ("taker", encode(&a.auth[shard].0)),
            ("slippageBps", "200".to_string()),
            ("maxAccounts", cap.to_string()),
            ("dexes", DEXES.to_string()),
        ];
        if case.native {
            query.push(("nativeDestinationAccount", encode(wallet)));
        } else {
            query.push(("destinationTokenAccount", encode(&output)));
        }
        let build = match f.build(&query) {
            Ok(b) => b,
            Err(e) => {
                outcome["build_error"] = json!(e);
                continue;
            }
        };
        let (mut route, jupiter) = route_of(&build);
        if !case.native && jupiter[..8] == swapic_solana_wire::jupiter::ROUTE_V2 {
            ixs::route_straight_to(
                &mut route,
                &pda::ata(&a.auth[shard].0, &mint, &case.token_program),
                &output,
            );
            outcome["straight"] = json!(true);
        }
        outcome["swapic_in_route"] = json!(swapic_in_route(a, &build));
        outcome["extra"] = json!(setup_of(&build));
        let min_out: u64 = build["otherAmountThreshold"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let hook = SolanaEnvelope {
            ref_bytes: r,
            action: SOLANA_ACTION_SWAP,
            deadline: (f.now() + 3600) as u32,
            recipient: *wallet,
            mint_out: mint,
            min_out,
        };
        let (nonce, message, attestation) = craft(f, shard, a, &hook.encode());
        let data = ArrivalArgs {
            tag: TAG_COMPLETE,
            message: &message,
            attestation: &attestation,
            jupiter: &jupiter,
        }
        .encode();
        let source = ArrivalSource {
            nonce: &nonce,
            source_domain: 6,
            source_token: &source_token,
        };
        let complete = ixs::complete(a, &a.canister, shard, &output, source, &route, data);

        let mut all = Vec::new();
        if case.native {
            all.push(ixs::create_ata_idempotent(
                &a.canister,
                &a.auth[shard].0,
                &WSOL_MINT,
                &TOKEN_PROGRAM,
            ));
        } else {
            all.push(ixs::create_ata_idempotent(
                &a.canister,
                wallet,
                &mint,
                &case.token_program,
            ));
        }
        all.push(complete);

        let config = V1Config {
            priority_lamports: 1000,
            compute_unit_limit: 1_400_000,
            loaded_data_limit: 64 * 1024 * 1024,
        };
        let compiled = match compile_v1(&a.canister, &all, &f.blockhash(), config) {
            Ok(m) => m,
            Err(e) => {
                outcome[format!("cap{cap}")] = json!(format!("{e:?}"));
                continue;
            }
        };
        let mut signatures = vec![[0u8; 64]; compiled.header.required_signatures as usize];
        for s in &mut signatures {
            rand::thread_rng().fill_bytes(s);
        }
        let wire = wire_v1(&compiled.encode(), &signatures);
        if wire.len() > MAX_V1_TX_BYTES {
            outcome[format!("cap{cap}")] = json!({"size": wire.len()});
            continue;
        }
        let wsol = a.wsol[shard];
        let wsol_rent_before = f.lamports(&wsol);
        let out_before = if case.native {
            f.lamports(wallet)
        } else {
            f.account(&output).map(|_| f.amount(&output)).unwrap_or(0)
        };
        let vault_before = f.amount(&a.vault[shard]);
        let landed = f.send(&wire);
        outcome["fits"] = json!(true);
        outcome["cap"] = json!(cap);
        outcome["size"] = json!(wire.len());
        outcome["accounts"] = json!(compiled.addresses.len());
        outcome["route"] = json!(labels_of(&build));
        outcome["min_out"] = json!(min_out);
        match landed {
            Ok(l) => {
                outcome["cu"] = json!(l.cu);
                outcome["own_cu"] = json!(swapic_solana_tests::own_compute(&l.logs, &a.program));
                outcome["err"] = json!(l.err);
                if l.err.is_none() {
                    assert_eq!(
                        f.amount(&a.vault[shard]),
                        vault_before,
                        "{}: the vault is not at its baseline",
                        case.label
                    );
                    let got = if case.native {
                        f.lamports(wallet) - out_before
                    } else {
                        f.amount(&output) - out_before
                    };
                    outcome["got"] = json!(got);
                    assert!(got >= min_out, "{}: {got} below {min_out}", case.label);
                    if case.native {
                        let open = f.account(&wsol).is_some();
                        outcome["wsol_after"] = json!(if open { "open" } else { "closed" });
                        outcome["wsol_lamports"] = json!([wsol_rent_before, f.lamports(&wsol)]);
                    }
                } else {
                    outcome["tail"] =
                        json!(l.logs.iter().rev().take(6).cloned().collect::<Vec<_>>());
                }
            }
            Err(e) => outcome["send_error"] = json!(e),
        }
        break;
    }
    f.record(outcome);
}

/// Jeff's question: can a `route_v2` arrival skip the lane's own account for the token? Takes the
/// route Jupiter answered and points its output straight at the user's account.
fn run_direct(
    f: &mut Fork,
    a: &SwapicAddresses,
    wallet: &Pubkey,
    label: &str,
    mint_text: &str,
    token_program: Pubkey,
    dexes: &str,
) {
    use swapic_solana_wire::jupiter::{
        ROUTE_V2, ROUTE_V2_DESTINATION, ROUTE_V2_DESTINATION_OPTIONAL,
    };
    let minted = ARRIVAL_AMOUNT - ARRIVAL_FEE;
    let mint = base58::decode_pubkey(mint_text).unwrap();
    let output = pda::ata(wallet, &mint, &token_program);
    let source_token = {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(&swapic_solana_tests::BASE_USDC);
        w
    };
    let mut seen_shared = 0;
    for _ in 0..2 {
        let r = random_ref();
        let shard = shard_of(&r);
        let query = vec![
            ("inputMint", USDC.to_string()),
            ("outputMint", mint_text.to_string()),
            ("amount", minted.to_string()),
            ("taker", encode(&a.auth[shard].0)),
            ("slippageBps", "200".to_string()),
            ("maxAccounts", "48".to_string()),
            ("destinationTokenAccount", encode(&output)),
            ("dexes", dexes.to_string()),
        ];
        let build = match f.build(&query) {
            Ok(b) => b,
            Err(e) => {
                f.record(json!({"direct": label, "build_error": e}));
                return;
            }
        };
        let (mut route, jupiter) = route_of(&build);
        if jupiter[..8] != ROUTE_V2 {
            seen_shared += 1;
            continue;
        }
        let lane_account = pda::ata(&a.auth[shard].0, &mint, &token_program);
        let before_rewrite: Vec<String> = route[..10]
            .iter()
            .map(|m| encode(&m.pubkey)[..6].to_string())
            .collect();
        let mut swapped = 0;
        for meta in route.iter_mut() {
            if meta.pubkey == lane_account {
                meta.pubkey = output;
                swapped += 1;
            }
        }
        // no forwarding step any more: the optional destination reads as Jupiter's own id
        let destination_was = encode(&route[ROUTE_V2_DESTINATION].pubkey);
        if route[ROUTE_V2_DESTINATION_OPTIONAL].pubkey == output {
            route[ROUTE_V2_DESTINATION_OPTIONAL] = AccountMeta::readonly(JUPITER);
        }
        let min_out: u64 = build["otherAmountThreshold"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let hook = SolanaEnvelope {
            ref_bytes: r,
            action: SOLANA_ACTION_SWAP,
            deadline: (f.now() + 3600) as u32,
            recipient: *wallet,
            mint_out: mint,
            min_out,
        };
        let (nonce, message, attestation) = craft(f, shard, a, &hook.encode());
        let data = ArrivalArgs {
            tag: TAG_COMPLETE,
            message: &message,
            attestation: &attestation,
            jupiter: &jupiter,
        }
        .encode();
        let source = ArrivalSource {
            nonce: &nonce,
            source_domain: 6,
            source_token: &source_token,
        };
        let complete = ixs::complete(a, &a.canister, shard, &output, source, &route, data);
        let create = ixs::create_ata_idempotent(&a.canister, wallet, &mint, &token_program);
        let wire = f.v1(&a.canister, &[create, complete]);
        let before = f.account(&output).map(|_| f.amount(&output)).unwrap_or(0);
        let landed = f.send(&wire);
        let lane_exists = f.account(&lane_account).is_some();
        match landed {
            Ok(l) => {
                let got = if l.err.is_none() {
                    f.amount(&output) - before
                } else {
                    0
                };
                let fail = l
                    .logs
                    .iter()
                    .rev()
                    .find(|x| x.contains("failed") || x.contains("Error"))
                    .cloned();
                f.record(json!({"direct": label, "route": labels_of(&build), "swapped": swapped, "first_accounts": before_rewrite, "destination_slot_was": destination_was, "err": l.err, "got": got, "min_out": min_out, "lane_account_exists": lane_exists, "cu": l.cu, "fail": fail}));
                if l.err.is_none() {
                    assert!(got >= min_out);
                }
            }
            Err(e) => f.record(json!({"direct": label, "send_error": e})),
        }
        return;
    }
    f.record(json!({"direct": label, "only_way_1_answers": seen_shared}));
}

fn run_release(f: &mut Fork, a: &SwapicAddresses, wallet: &Pubkey, stranger: &Pubkey) {
    let r = random_ref();
    let shard = shard_of(&r);
    let hook = SolanaEnvelope {
        ref_bytes: r,
        action: SOLANA_ACTION_SWAP,
        deadline: (f.now() + 30) as u32,
        recipient: *wallet,
        mint_out: base58::decode_pubkey(BONK).unwrap(),
        min_out: 1,
    };
    let (nonce, message, attestation) = craft(f, shard, a, &hook.encode());
    let data = ArrivalArgs {
        tag: TAG_RELEASE,
        message: &message,
        attestation: &attestation,
        jupiter: &[],
    }
    .encode();
    let token = {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(&swapic_solana_tests::BASE_USDC);
        w
    };
    let source = ArrivalSource {
        nonce: &nonce,
        source_domain: 6,
        source_token: &token,
    };
    let usdc_ata = pda::ata(wallet, &USDC_MINT, &TOKEN_PROGRAM);
    let release = ixs::release(
        a,
        &a.canister,
        shard,
        shard,
        &usdc_ata,
        source,
        data.clone(),
    );
    let create = ixs::create_ata_idempotent(&a.canister, wallet, &USDC_MINT, &TOKEN_PROGRAM);
    let before = f
        .account(&usdc_ata)
        .map(|_| f.amount(&usdc_ata))
        .unwrap_or(0);
    let wire = f.v1(&a.canister, &[create, release]);
    let landed = f.send(&wire).expect("the release sends");
    let got = f.amount(&usdc_ata) - before;
    f.record(json!({"case": "release by the canister", "err": landed.err, "cu": landed.cu, "own_cu": swapic_solana_tests::own_compute(&landed.logs, &a.program), "size": wire.len(), "got": got}));
    assert!(landed.err.is_none());
    assert_eq!(got, ARRIVAL_AMOUNT - ARRIVAL_FEE);

    // a stranger before the deadline is refused, after it is allowed
    let r = random_ref();
    let shard = shard_of(&r);
    let hook = SolanaEnvelope {
        ref_bytes: r,
        action: SOLANA_ACTION_SWAP,
        deadline: (f.now() + 2) as u32,
        recipient: *wallet,
        mint_out: base58::decode_pubkey(BONK).unwrap(),
        min_out: 1,
    };
    let (nonce, message, attestation) = craft(f, shard, a, &hook.encode());
    let data = ArrivalArgs {
        tag: TAG_RELEASE,
        message: &message,
        attestation: &attestation,
        jupiter: &[],
    }
    .encode();
    let source = ArrivalSource {
        nonce: &nonce,
        source_domain: 6,
        source_token: &token,
    };
    std::thread::sleep(Duration::from_secs(4));
    let release = ixs::release(a, stranger, shard, shard, &usdc_ata, source, data);
    let wire = f.v1(stranger, &[release]);
    let landed = f.send(&wire).expect("the late release sends");
    f.record(json!({"case": "release by a stranger after the deadline", "err": landed.err, "cu": landed.cu}));
    assert!(landed.err.is_none());
}

fn main() {
    let report = std::env::args().nth(1).expect("a report path");
    let only = std::env::args().nth(2).unwrap_or_default();
    let so = format!(
        "{}/../target/deploy/swapic_solana_program.so",
        env!("CARGO_MANIFEST_DIR")
    );

    let canister = key_of("swapic-test-canister");
    let admin = key_of("swapic-test-admin");
    let program = key_of("swapic-test-program");
    let a = SwapicAddresses::derive(program, canister, admin);
    let user = key_of("fork-user");
    let stranger = key_of("fork-stranger");
    let wallet = key_of("fork-wallet");

    let mut attesters: Vec<SigningKey> = ["fork-attester-a", "fork-attester-b"]
        .iter()
        .map(|label| {
            let seed: [u8; 32] = Sha256::digest(label.as_bytes()).into();
            SigningKey::from_bytes((&seed).into()).unwrap()
        })
        .collect();
    let address_of = |key: &SigningKey| -> [u8; 20] {
        let point = key.verifying_key().to_encoded_point(false);
        keccak256(&point.as_bytes()[1..])[12..].try_into().unwrap()
    };
    attesters.sort_by_key(address_of);
    let mut f = Fork {
        last_jupiter: std::time::Instant::now() - Duration::from_secs(10),
        attesters,
        log: Vec::new(),
    };

    // the program, as the research placed it
    let bytes = std::fs::read(&so).expect("the program is built by fork/run.sh");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    f.result("surfnet_setAccount", json!([encode(&program), {"lamports": 1_000_000_000u64, "data": hex, "owner": "BPFLoader2111111111111111111111111111111111", "executable": true}]));

    // the attesters, in the forked state
    let (lamports, mut data, owner) = f.account(&MESSAGE_TRANSMITTER).expect("the transmitter");
    let threshold = u32::from_le_bytes(data[145..149].try_into().unwrap());
    let count = u32::from_le_bytes(data[149..153].try_into().unwrap()) as usize;
    f.record(json!({"live_attesters": count, "live_threshold": threshold}));
    assert_eq!(threshold, 2, "the live threshold changed: read design 7.1");
    for i in 0..count {
        data[153 + 32 * i..185 + 32 * i].fill(0);
    }
    for (i, key) in f.attesters.iter().enumerate() {
        data[153 + 32 * i + 12..185 + 32 * i].copy_from_slice(&address_of(key));
    }
    let hexed: String = data.iter().map(|b| format!("{b:02x}")).collect();
    f.result("surfnet_setAccount", json!([encode(&MESSAGE_TRANSMITTER), {"lamports": lamports, "owner": encode(&owner), "executable": false, "data": hexed}]));

    for key in [canister, admin, user, stranger, wallet, a.rent.0] {
        f.set_lamports(&key, 500_000_000_000);
    }
    // attack L10: both directions registered, for every Swapic domain, on the live state
    let mut missing = Vec::new();
    for chain in &SWAPIC_DOMAINS {
        let mut token = [0u8; 32];
        token[12..].copy_from_slice(&chain.usdc);
        if f.account(&pda::remote_token_messenger(chain.domain))
            .is_none()
            || f.account(&pda::token_pair(chain.domain, &token)).is_none()
        {
            missing.push(chain.domain);
        }
    }
    f.record(json!({"domains_missing_on_solana": missing}));

    // the deploy session: accounts, config, table
    let mut setup: Vec<Instruction> = Vec::new();
    for shard in 0..SHARDS {
        setup.push(ixs::create_ata_idempotent(
            &canister,
            &a.auth[shard].0,
            &USDC_MINT,
            &TOKEN_PROGRAM,
        ));
        setup.push(ixs::create_ata_idempotent(
            &canister,
            &a.auth[shard].0,
            &WSOL_MINT,
            &TOKEN_PROGRAM,
        ));
    }
    setup.push(ixs::create_ata_idempotent(
        &canister,
        &admin,
        &USDC_MINT,
        &TOKEN_PROGRAM,
    ));
    f.must_land("the vaults", &canister, &setup);
    let swap_domains = SWAPIC_DOMAINS
        .iter()
        .fold(0u64, |mask, c| mask | 1 << c.domain);
    let config = Config {
        paused: false,
        event_keypair_allowed: false,
        bump: 0,
        swap_domains,
        ck_domains: 1,
        min_burn: 1_000_000,
        min_deadline_s: 600,
        max_deadline_s: 7200,
    };
    f.must_land("set_config", &admin, &[ixs::set_config(&a, &config)]);

    let slot = f.slot();
    let (create, table_address) = ixs::create_lookup_table(&admin, &admin, slot.saturating_sub(1));
    let entries = ixs::swapic_table_entries(&a);
    // the table program refuses instruction data over 1,232 bytes: two extends in the one transaction
    let mut table_ixs = vec![create];
    table_ixs.extend(
        entries
            .chunks(30)
            .map(|chunk| ixs::extend_lookup_table(&table_address, &admin, &admin, chunk)),
    );
    table_ixs.push(ixs::freeze_lookup_table(&table_address, &admin));
    let wire = f.v1(&admin, &table_ixs);
    let landed = f.send(&wire).expect("the table");
    f.record(json!({"table": encode(&table_address), "entries": entries.len(), "size": wire.len(), "err": landed.err, "cu": landed.cu}));
    assert!(landed.err.is_none(), "{}", landed.logs.join("\n"));
    std::thread::sleep(Duration::from_secs(2));
    let table = LookupTable {
        address: table_address,
        entries,
    };

    // tokens for the user: amounts the fork's cheat code writes
    for (mint, amount) in [
        (BONK, 400_000_000_000_000u64),
        (JUP, 50_000_000_000),
        (WIF, 80_000_000_000),
        (USDC, 5_000_000_000),
    ] {
        f.result(
            "surfnet_setTokenAccount",
            json!([encode(&user), mint, {"amount": amount, "state": "initialized"}, encode(&TOKEN_PROGRAM)]),
        );
    }

    // the services' run (fork/run_services.sh): the fork is left as it is for the real quoter and watcher,
    // with attested messages they could not get any other way, and held until they are done
    if only == "hold" {
        let mut crafted = Vec::new();
        for (mint, label) in [(BONK, "bonk"), (SOL, "sol")] {
            let r = random_ref();
            let shard = shard_of(&r);
            let mint_key = base58::decode_pubkey(mint).unwrap();
            let hook = SolanaEnvelope {
                ref_bytes: r,
                action: SOLANA_ACTION_SWAP,
                deadline: (f.now() + 3600) as u32,
                recipient: wallet,
                mint_out: mint_key,
                min_out: 1,
            };
            let (nonce, message, attestation) = craft(&f, shard, &a, &hook.encode());
            let hex_of = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
            crafted.push(json!({"label": label, "shard": shard, "mint": mint, "nonce": hex_of(&nonce), "message": hex_of(&message), "attestation": hex_of(&attestation)}));
        }
        let hold = json!({
            "program": encode(&program), "canister": encode(&canister), "admin": encode(&admin),
            "user": encode(&user), "wallet": encode(&wallet),
            "table": encode(&table.address), "entries": table.entries.iter().map(|e| encode(e)).collect::<Vec<_>>(),
            "arrivals": crafted, "minted": ARRIVAL_AMOUNT - ARRIVAL_FEE,
        });
        std::fs::write(&report, serde_json::to_string_pretty(&hold).unwrap()).unwrap();
        let stop = format!("{report}.stop");
        while !std::path::Path::new(&stop).exists() {
            std::thread::sleep(Duration::from_secs(1));
        }
        return;
    }

    let want = |name: &str| only.is_empty() || name.contains(&only);
    let memo = [0x5au8; 180];
    let principal = [0x77u8; 32];
    let outs = [
        OutCase {
            label: "out user pays SOL",
            mint: SOL,
            amount: "11160000000",
            gasless: false,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out user pays BONK",
            mint: BONK,
            amount: "35700000000000",
            gasless: false,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out user pays JUP",
            mint: JUP,
            amount: "4125783593",
            gasless: false,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out user pays WIF",
            mint: WIF,
            amount: "6222118407",
            gasless: false,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out gasless BONK",
            mint: BONK,
            amount: "35700000000000",
            gasless: true,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out gasless JUP",
            mint: JUP,
            amount: "4125783593",
            gasless: true,
            memo: None,
            ck_principal: None,
        },
        OutCase {
            label: "out to ICP with the memo, user pays BONK",
            mint: BONK,
            amount: "35700000000000",
            gasless: false,
            memo: Some(&memo),
            ck_principal: None,
        },
        OutCase {
            label: "out to ICP with the memo, gasless JUP",
            mint: JUP,
            amount: "4125783593",
            gasless: true,
            memo: Some(&[0x5b; 180]),
            ck_principal: None,
        },
        OutCase {
            label: "out ck deposit to Ethereum, user pays BONK",
            mint: BONK,
            amount: "35700000000000",
            gasless: false,
            memo: None,
            ck_principal: Some(&principal),
        },
    ];
    for case in &outs {
        if want(case.label) {
            run_out(&mut f, &a, &table, &user, case);
        }
    }

    // `burn`, with the USDC the user holds
    if want("burn usdc") {
        let r = random_ref();
        let shard = shard_of(&r);
        let event = a.event(&r);
        let args = BurnArgs {
            tag: TAG_BURN,
            fast: true,
            event_keypair: false,
            float: false,
            domain: 6,
            ref_source: RefSource::Direct(&r),
            deadline: (f.now() + 1800) as u32,
            recipient: &[0x11; 20],
            payload: Payload::Deliver,
            min_gas: 0,
            max_fee_deci_bps: 13,
            amount: 100_000_000,
            fee: 50_000,
            jupiter: &[],
        };
        let head = ixs::burn_head(&a, &user, shard, 6, false, &event, false);
        let ix = ixs::burn(
            &a,
            head,
            &pda::ata(&user, &USDC_MINT, &TOKEN_PROGRAM),
            args.encode(),
        );
        let all = [
            ixs::compute_unit_limit(300_000),
            ixs::compute_unit_price(1000),
            ix,
        ];
        let message =
            compile_v0(&user, &all, std::slice::from_ref(&table), &f.blockhash()).unwrap();
        let wire = sign_message_v0(&message.encode(), 1);
        let landed = f.send(&wire).expect("burn sends");
        f.record(json!({"case": "burn usdc, deliver to Base", "size": wire.len(), "accounts": message.total_accounts(), "err": landed.err, "cu": landed.cu, "own_cu": swapic_solana_tests::own_compute(&landed.logs, &a.program)}));
    }

    if want("direct") {
        for dexes in [
            "Raydium CLMM,Whirlpool",
            "Meteora DLMM,Whirlpool",
            "Raydium CLMM,Meteora DLMM",
            "Raydium,Whirlpool",
            DEXES,
        ] {
            run_direct(
                &mut f,
                &a,
                &wallet,
                "PYUSD",
                PYUSD,
                TOKEN_2022_PROGRAM,
                dexes,
            );
            run_direct(&mut f, &a, &wallet, "BONK", BONK, TOKEN_PROGRAM, dexes);
            run_direct(&mut f, &a, &wallet, "JUP", JUP, TOKEN_PROGRAM, dexes);
            run_direct(&mut f, &a, &wallet, "WIF", WIF, TOKEN_PROGRAM, dexes);
        }
    }
    let arrivals = [
        ArrivalCase {
            label: "arrival USDC to BONK",
            mint: BONK,
            token_program: TOKEN_PROGRAM,
            native: false,
        },
        ArrivalCase {
            label: "arrival USDC to JUP",
            mint: JUP,
            token_program: TOKEN_PROGRAM,
            native: false,
        },
        ArrivalCase {
            label: "arrival USDC to PYUSD (Token-2022)",
            mint: PYUSD,
            token_program: TOKEN_2022_PROGRAM,
            native: false,
        },
        ArrivalCase {
            label: "arrival USDC to native SOL",
            mint: SOL,
            token_program: TOKEN_PROGRAM,
            native: true,
        },
        ArrivalCase {
            label: "arrival USDC to native SOL again",
            mint: SOL,
            token_program: TOKEN_PROGRAM,
            native: true,
        },
    ];
    for case in &arrivals {
        if want(case.label) {
            run_arrival(&mut f, &a, &wallet, case);
        }
    }
    if want("release") {
        run_release(&mut f, &a, &wallet, &stranger);
    }
    std::fs::write(&report, serde_json::to_string_pretty(&f.log).unwrap()).unwrap();
}
