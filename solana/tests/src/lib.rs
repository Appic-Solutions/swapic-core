//! The harness of the program's tests: the real CCTP programs and accounts dumped from mainnet (with
//! two throwaway attesters patched in), the program built with test constants, and a stand-in router at
//! Jupiter's address that is honest or hostile on request.

use base64::Engine;
use k256::ecdsa::SigningKey;
use litesvm::types::{FailedTransactionMetadata, TransactionMetadata};
use litesvm::LiteSVM;
use sha2::{Digest, Sha256};
use solana_account::Account;
use solana_address::Address;
use solana_clock::Clock;
use solana_instruction::{AccountMeta as SdkMeta, Instruction as SdkInstruction};
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_message::Message as LegacyMessage;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
pub use swapic_solana_wire as wire;
use wire::args::Config;
use wire::base58;
use wire::cctp::{self};
use wire::consts::*;
use wire::instructions;
use wire::message::Instruction;
use wire::pda::{self, keccak256, SwapicAddresses};
use wire::Pubkey;

pub const NOW: i64 = 1_800_000_000;
pub const ONE_USDC: u64 = 1_000_000;

/// A key from a labelled seed: the same label always gives the same throwaway key.
pub fn test_key(label: &str) -> Keypair {
    let seed: [u8; 32] = Sha256::digest(label.as_bytes()).into();
    Keypair::new_from_array(seed)
}

pub fn pubkey_of(key: &Keypair) -> Pubkey {
    key.pubkey().to_bytes()
}

fn address(key: &Pubkey) -> Address {
    Address::new_from_array(*key)
}

/// The classic token account bytes, 165 of them.
pub fn token_account_bytes(
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    native_rent: Option<u64>,
) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[..32].copy_from_slice(mint);
    data[32..64].copy_from_slice(owner);
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    if let Some(rent) = native_rent {
        data[109] = 1;
        data[113..121].copy_from_slice(&rent.to_le_bytes());
    }
    data
}

fn mint_bytes(decimals: u8) -> Vec<u8> {
    let mut data = vec![0u8; 82];
    data[36..44].copy_from_slice(&u64::MAX.to_le_bytes());
    data[44] = decimals;
    data[45] = 1;
    data
}

pub struct Env {
    pub svm: LiteSVM,
    pub a: SwapicAddresses,
    pub canister: Keypair,
    pub admin: Keypair,
    pub user: Keypair,
    pub attesters: Vec<SigningKey>,
    /// The router's PDA, which owns its pools.
    pub pool: Pubkey,
    pub mint_in: Pubkey,
    pub mint_out: Pubkey,
    pub user_in: Pubkey,
    pub user_usdc: Pubkey,
    pub pool_in: Pubkey,
    pub pool_usdc: Pubkey,
    pub pool_out: Pubkey,
    pub pool_wsol: Pubkey,
    pub recipient: Keypair,
    pub recipient_out: Pubkey,
    pub recipient_usdc: Pubkey,
    pub stranger: Keypair,
    pub evm_recipient: [u8; 20],
}

pub const BASE_DOMAIN: u8 = 6;
pub const ETHEREUM_DOMAIN: u8 = 0;
pub const BASE_USDC: [u8; 20] = [
    0x83, 0x35, 0x89, 0xfc, 0xd6, 0xed, 0xb6, 0xe0, 0x8f, 0x4c, 0x7c, 0x32, 0xd4, 0xf7, 0x1b, 0x54,
    0xbd, 0xa0, 0x29, 0x13,
];
/// The Base TokenMessengerV2, as Solana's `remote_token_messenger(6)` records it.
pub const BASE_TOKEN_MESSENGER: [u8; 20] = [
    0x28, 0xb5, 0xa0, 0xe9, 0xc6, 0x21, 0xa5, 0xba, 0xda, 0xa5, 0x36, 0x21, 0x9b, 0x3a, 0x22, 0x8c,
    0x81, 0x68, 0xcf, 0x5d,
];
pub const CCTP_RECEIVER: [u8; 20] = [
    0x40, 0x40, 0x56, 0xab, 0x78, 0x1d, 0x71, 0x3b, 0x8f, 0x5d, 0xc5, 0x5f, 0x5b, 0x95, 0x92, 0x96,
    0x2b, 0x18, 0x70, 0x99,
];

fn so_path(name: &str) -> String {
    format!("{}/../target/deploy/{name}.so", env!("CARGO_MANIFEST_DIR"))
}

fn fixture_account(key: &Pubkey) -> Account {
    let path = format!(
        "{}/fixtures/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        base58::encode(key)
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}; run the fetch_fixtures binary"));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let account = &v["account"];
    let data = base64::engine::general_purpose::STANDARD
        .decode(account["data"][0].as_str().unwrap())
        .unwrap();
    Account {
        lamports: account["lamports"].as_u64().unwrap(),
        data,
        owner: address(&base58::decode_pubkey(account["owner"].as_str().unwrap()).unwrap()),
        executable: account["executable"].as_bool().unwrap(),
        rent_epoch: u64::MAX,
    }
}

fn evm_address(key: &SigningKey) -> [u8; 20] {
    let point = key.verifying_key().to_encoded_point(false);
    let hash = keccak256(&point.as_bytes()[1..]);
    hash[12..].try_into().unwrap()
}

pub fn word(evm: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(evm);
    out
}

fn u256_word(value: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

impl Env {
    /// A world ready for both directions, config set, program unpaused.
    pub fn new() -> Self {
        Self::with_program("swapic_solana_program")
    }

    pub fn with_program(so: &str) -> Self {
        let mut svm = LiteSVM::new().with_sigverify(true);
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = NOW;
        svm.set_sysvar(&clock);

        let canister = test_key("swapic-test-canister");
        let admin = test_key("swapic-test-admin");
        let program = test_key("swapic-test-program");
        let a =
            SwapicAddresses::derive(pubkey_of(&program), pubkey_of(&canister), pubkey_of(&admin));

        svm.add_program_from_file(address(&a.program), so_path(so))
            .expect("the program is built by tests/run.sh");
        svm.add_program_from_file(address(&JUPITER), so_path("swapic_test_router"))
            .expect("the router");
        let fixtures = format!("{}/fixtures", env!("CARGO_MANIFEST_DIR"));
        svm.add_program_from_file(
            address(&MT),
            format!("{fixtures}/message_transmitter_v2.so"),
        )
        .unwrap();
        svm.add_program_from_file(
            address(&TMM),
            format!("{fixtures}/token_messenger_minter_v2.so"),
        )
        .unwrap();

        let base_token = word(&BASE_USDC);
        for key in [
            TOKEN_MESSENGER,
            TOKEN_MINTER,
            LOCAL_TOKEN,
            USDC_MINT,
            CUSTODY,
            FEE_RECIPIENT_TOKEN_ACCOUNT,
            pda::remote_token_messenger(6),
            pda::remote_token_messenger(0),
            pda::token_pair(6, &base_token),
        ] {
            svm.set_account(address(&key), fixture_account(&key))
                .unwrap();
        }

        // two throwaway attesters, sorted by address as CCTP wants their signatures
        let mut attesters: Vec<SigningKey> = ["attester-a", "attester-b"]
            .iter()
            .map(|label| {
                let seed: [u8; 32] = Sha256::digest(label.as_bytes()).into();
                SigningKey::from_bytes((&seed).into()).unwrap()
            })
            .collect();
        attesters.sort_by_key(evm_address);
        let mut transmitter = fixture_account(&MESSAGE_TRANSMITTER);
        let d = &mut transmitter.data;
        assert_eq!(
            u32::from_le_bytes(d[145..149].try_into().unwrap()),
            2,
            "the threshold on mainnet changed"
        );
        let count = u32::from_le_bytes(d[149..153].try_into().unwrap()) as usize;
        for i in 0..count {
            d[153 + 32 * i..185 + 32 * i].fill(0);
        }
        for (i, key) in attesters.iter().enumerate() {
            d[153 + 32 * i + 12..185 + 32 * i].copy_from_slice(&evm_address(key));
        }
        svm.set_account(address(&MESSAGE_TRANSMITTER), transmitter)
            .unwrap();

        let user = test_key("user");
        let recipient = test_key("recipient");
        let stranger = test_key("stranger");
        for key in [&user, &recipient, &stranger, &canister, &admin] {
            svm.airdrop(&key.pubkey(), 100_000_000_000).unwrap();
        }
        let pool = pda::find_program_address(&[b"pool"], &JUPITER).0;
        let mint_in = pubkey_of(&test_key("mint-in"));
        let mint_out = pubkey_of(&test_key("mint-out"));
        let (user_key, recipient_key) = (pubkey_of(&user), pubkey_of(&recipient));
        let mut env = Self {
            svm,
            canister,
            admin,
            user,
            attesters,
            pool,
            mint_in,
            mint_out,
            user_in: pda::ata(&user_key, &mint_in, &TOKEN_PROGRAM),
            user_usdc: pda::ata(&user_key, &USDC_MINT, &TOKEN_PROGRAM),
            pool_in: pubkey_of(&test_key("pool-in")),
            pool_usdc: pubkey_of(&test_key("pool-usdc")),
            pool_out: pubkey_of(&test_key("pool-out")),
            pool_wsol: pubkey_of(&test_key("pool-wsol")),
            recipient,
            recipient_out: pda::ata(&recipient_key, &mint_out, &TOKEN_PROGRAM),
            recipient_usdc: pda::ata(&recipient_key, &USDC_MINT, &TOKEN_PROGRAM),
            stranger,
            evm_recipient: [0x11; 20],
            a,
        };
        env.put_mint(&mint_in, 6);
        env.put_mint(&mint_out, 9);
        env.put_mint(&WSOL_MINT, 9);
        for shard in 0..SHARDS {
            let (auth, vault, wsol) = (env.a.auth[shard].0, env.a.vault[shard], env.a.wsol[shard]);
            env.put_token(&vault, &USDC_MINT, &auth, 0);
            env.put_wsol(&wsol, &auth, 0);
            // no lamports at rest: `auth` is a bare address
            assert!(env.svm.get_account(&address(&auth)).is_none());
        }
        let fee = env.a.fee_account;
        let admin_key = pubkey_of(&env.admin);
        env.put_token(&fee, &USDC_MINT, &admin_key, 0);
        // the float
        env.fund(&env.a.rent.0.clone(), 5_000_000_000);
        // user and pools
        let (pool, pool_in, pool_usdc, pool_out, pool_wsol) = (
            env.pool,
            env.pool_in,
            env.pool_usdc,
            env.pool_out,
            env.pool_wsol,
        );
        let (user_in, user_usdc, recipient_out, recipient_usdc) = (
            env.user_in,
            env.user_usdc,
            env.recipient_out,
            env.recipient_usdc,
        );
        env.put_token(&user_in, &mint_in, &user_key, 1_000_000 * ONE_USDC);
        env.put_token(&user_usdc, &USDC_MINT, &user_key, 10_000 * ONE_USDC);
        env.put_token(&pool_in, &mint_in, &pool, 0);
        env.put_token(&pool_usdc, &USDC_MINT, &pool, 1_000_000 * ONE_USDC);
        env.put_token(&pool_out, &mint_out, &pool, 1_000_000 * ONE_USDC);
        env.put_wsol(&pool_wsol, &pool, 1_000_000 * ONE_USDC);
        env.fund(&pool, 100_000_000_000);
        env.put_token(&recipient_out, &mint_out, &recipient_key, 0);
        env.put_token(&recipient_usdc, &USDC_MINT, &recipient_key, 0);
        env.send_ok(
            &[instructions::set_config(&env.a.clone(), &default_config())],
            &env.admin.insecure_clone(),
            &[],
        );
        env
    }

    pub fn put_mint(&mut self, key: &Pubkey, decimals: u8) {
        let data = mint_bytes(decimals);
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(
                address(key),
                Account {
                    lamports,
                    data,
                    owner: address(&TOKEN_PROGRAM),
                    executable: false,
                    rent_epoch: u64::MAX,
                },
            )
            .unwrap();
    }

    pub fn put_token(&mut self, key: &Pubkey, mint: &Pubkey, owner: &Pubkey, amount: u64) {
        let data = token_account_bytes(mint, owner, amount, None);
        let lamports = self.svm.minimum_balance_for_rent_exemption(165);
        self.svm
            .set_account(
                address(key),
                Account {
                    lamports,
                    data,
                    owner: address(&TOKEN_PROGRAM),
                    executable: false,
                    rent_epoch: u64::MAX,
                },
            )
            .unwrap();
    }

    pub fn put_wsol(&mut self, key: &Pubkey, owner: &Pubkey, amount: u64) {
        let rent = self.svm.minimum_balance_for_rent_exemption(165);
        let data = token_account_bytes(&WSOL_MINT, owner, amount, Some(rent));
        self.svm
            .set_account(
                address(key),
                Account {
                    lamports: rent + amount,
                    data,
                    owner: address(&TOKEN_PROGRAM),
                    executable: false,
                    rent_epoch: u64::MAX,
                },
            )
            .unwrap();
    }

    pub fn fund(&mut self, key: &Pubkey, lamports: u64) {
        self.svm.airdrop(&address(key), lamports).unwrap();
    }

    /// Writes a field of an account by hand: a test's stand-in for a state nothing else can reach.
    pub fn poke(&mut self, key: &Pubkey, change: impl FnOnce(&mut Account)) {
        let mut account = self
            .svm
            .get_account(&address(key))
            .expect("the account exists");
        change(&mut account);
        self.svm.set_account(address(key), account).unwrap();
    }

    pub fn lamports(&self, key: &Pubkey) -> u64 {
        self.svm
            .get_account(&address(key))
            .map(|a| a.lamports)
            .unwrap_or(0)
    }

    pub fn exists(&self, key: &Pubkey) -> bool {
        self.svm
            .get_account(&address(key))
            .is_some_and(|a| a.lamports > 0)
    }

    pub fn data(&self, key: &Pubkey) -> Vec<u8> {
        self.svm
            .get_account(&address(key))
            .map(|a| a.data)
            .unwrap_or_default()
    }

    pub fn balance(&self, key: &Pubkey) -> u64 {
        let data = self.data(key);
        u64::from_le_bytes(data[64..72].try_into().unwrap())
    }

    pub fn warp(&mut self, seconds: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp += seconds;
        self.svm.set_sysvar(&clock);
    }

    pub fn now(&self) -> i64 {
        let clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp
    }

    pub fn set_config(&mut self, change: impl FnOnce(&mut Config)) {
        let mut config = Config::parse(&self.data(&self.a.config.0)).unwrap();
        change(&mut config);
        let ix = instructions::set_config(&self.a.clone(), &config);
        let admin = self.admin.insecure_clone();
        self.send_ok(&[ix], &admin, &[]);
    }

    /// Sends one transaction signed by the payer and the other keys.
    pub fn send(
        &mut self,
        instructions: &[Instruction],
        payer: &Keypair,
        others: &[&Keypair],
    ) -> Result<TransactionMetadata, FailedTransactionMetadata> {
        // real arrivals take about 330k compute, over the default limit of 200k per instruction
        let mut sdk: Vec<SdkInstruction> =
            vec![to_sdk(&wire::instructions::compute_unit_limit(1_400_000))];
        sdk.extend(instructions.iter().map(to_sdk));
        self.svm.expire_blockhash();
        let message = LegacyMessage::new(&sdk, Some(&payer.pubkey()));
        let mut signers: Vec<&Keypair> = vec![payer];
        signers.extend_from_slice(others);
        let tx = Transaction::new(&signers, message, self.svm.latest_blockhash());
        self.svm.send_transaction(tx)
    }

    pub fn send_ok(
        &mut self,
        instructions: &[Instruction],
        payer: &Keypair,
        others: &[&Keypair],
    ) -> TransactionMetadata {
        match self.send(instructions, payer, others) {
            Ok(meta) => meta,
            Err(failed) => panic!(
                "the transaction failed: {:?}\n{}",
                failed.err,
                failed.meta.logs.join("\n")
            ),
        }
    }

    /// Writes a CCTP message for a burn from Base into Solana, with its attestation.
    pub fn craft(&self, spec: &MessageSpec<'_>) -> (Vec<u8>, Vec<u8>) {
        let mut m = vec![0u8; cctp::HOOK_AT];
        m[cctp::HEADER_VERSION].copy_from_slice(&spec.header_version.to_be_bytes());
        m[cctp::SOURCE_DOMAIN].copy_from_slice(&6u32.to_be_bytes());
        m[cctp::DESTINATION_DOMAIN].copy_from_slice(&spec.destination.to_be_bytes());
        m[cctp::NONCE].copy_from_slice(spec.nonce);
        m[cctp::SENDER].copy_from_slice(&word(&BASE_TOKEN_MESSENGER));
        m[cctp::RECIPIENT].copy_from_slice(&TMM);
        m[cctp::DESTINATION_CALLER].copy_from_slice(spec.caller);
        m[140..144].copy_from_slice(&1000u32.to_be_bytes());
        m[144..148].copy_from_slice(&1000u32.to_be_bytes());
        m[cctp::BODY_VERSION].copy_from_slice(&1u32.to_be_bytes());
        m[cctp::BURN_TOKEN].copy_from_slice(&word(&BASE_USDC));
        m[cctp::MINT_RECIPIENT].copy_from_slice(spec.mint_recipient);
        m[cctp::AMOUNT].copy_from_slice(&u256_word(spec.amount));
        m[cctp::MESSAGE_SENDER].copy_from_slice(&word(&[0xab; 20]));
        m[cctp::MAX_FEE].copy_from_slice(&u256_word(spec.fee));
        m[cctp::FEE_EXECUTED].copy_from_slice(&u256_word(spec.fee));
        m.extend_from_slice(spec.hook);
        let attestation = self.attest(&m);
        (m, attestation)
    }

    pub fn attest(&self, message: &[u8]) -> Vec<u8> {
        let hash = keccak256(message);
        let mut out = Vec::with_capacity(130);
        for key in &self.attesters {
            let (signature, recovery) = key.sign_prehash_recoverable(&hash).unwrap();
            out.extend_from_slice(&signature.to_bytes());
            out.push(27 + recovery.to_byte());
        }
        out
    }

    pub fn source(&self) -> (u32, [u8; 32]) {
        (6, word(&BASE_USDC))
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

/// The parts of a message a test changes.
pub struct MessageSpec<'a> {
    pub header_version: u32,
    pub destination: u32,
    pub nonce: &'a [u8; 32],
    pub caller: &'a [u8; 32],
    pub mint_recipient: &'a [u8; 32],
    pub amount: u64,
    pub fee: u64,
    pub hook: &'a [u8],
}

/// The launch config: Base and Ethereum open, 1 USDC least burn, the design's deadline window.
pub fn default_config() -> Config {
    Config {
        paused: false,
        event_keypair_allowed: false,
        bump: 0,
        swap_domains: 1 << BASE_DOMAIN | 1 << ETHEREUM_DOMAIN,
        ck_domains: 1,
        min_burn: ONE_USDC,
        min_deadline_s: 600,
        max_deadline_s: 7200,
    }
}

pub fn to_sdk(ix: &Instruction) -> SdkInstruction {
    SdkInstruction {
        program_id: address(&ix.program),
        accounts: ix
            .accounts
            .iter()
            .map(|m| SdkMeta {
                pubkey: address(&m.pubkey),
                is_signer: m.signer,
                is_writable: m.writable,
            })
            .collect(),
        data: ix.data.clone(),
    }
}

/// The custom code a transaction failed with, or a panic that shows the logs.
pub fn custom_code(result: Result<TransactionMetadata, FailedTransactionMetadata>) -> u32 {
    match result {
        Ok(meta) => panic!(
            "the transaction landed, it should have failed:\n{}",
            meta.logs.join("\n")
        ),
        Err(failed) => match failed.err {
            TransactionError::InstructionError(_, InstructionError::Custom(code)) => code,
            other => panic!(
                "not a custom error: {other:?}\n{}",
                failed.meta.logs.join("\n")
            ),
        },
    }
}

/// True when the failure is one the runtime or another program raised, not one of Swapic's codes.
pub fn is_foreign_failure(result: Result<TransactionMetadata, FailedTransactionMetadata>) -> bool {
    matches!(result, Err(_))
}

/// The compute Swapic's own code used: its total less what every program it called reports.
pub fn own_compute(logs: &[String], program: &Pubkey) -> u64 {
    let id = base58::encode(program);
    let mut depth = 0usize;
    let mut own = 0u64;
    let mut children = 0u64;
    let mut in_swapic = 0usize;
    for line in logs {
        if line.ends_with(&format!("invoke [{}]", depth + 1)) {
            depth += 1;
            if line.contains(&id) {
                in_swapic = depth;
            }
            continue;
        }
        if line.contains(" consumed ") && line.contains(" of ") {
            let used: u64 = line
                .split(" consumed ")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            if depth == in_swapic && line.contains(&id) {
                own += used.saturating_sub(children);
                children = 0;
            } else if depth == in_swapic + 1 {
                children += used;
            }
        }
        if line.ends_with(" success") || line.contains(" failed: ") {
            depth = depth.saturating_sub(1);
        }
    }
    own
}

pub mod route {
    //! The instruction data of a route the stand-in router reads.
    use swapic_solana_wire::jupiter::{ROUTE_V2, SHARED_ACCOUNTS_ROUTE_V2};

    pub fn data(in_amount: u64, quoted: u64, mode: u8, arg: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(43);
        out.extend_from_slice(&ROUTE_V2);
        out.extend_from_slice(&in_amount.to_le_bytes());
        out.extend_from_slice(&quoted.to_le_bytes());
        out.extend_from_slice(&50u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(mode);
        out.extend_from_slice(&arg.to_le_bytes());
        out
    }

    pub fn shared(in_amount: u64, quoted: u64, mode: u8) -> Vec<u8> {
        // the stand-in reads route_v2's offsets, so this form is only ever refused or accepted by discriminator
        let mut out = data(in_amount, quoted, mode, 0);
        out[..8].copy_from_slice(&SHARED_ACCOUNTS_ROUTE_V2);
        out
    }

    pub const HONEST: u8 = 0;
    pub const PAY_SHORT: u8 = 1;
    pub const SPEND_PART: u8 = 2;
    pub const APPROVE_DELEGATE: u8 = 3;
    pub const SET_OWNER: u8 = 4;
    pub const SET_CLOSE_AUTHORITY: u8 = 5;
    pub const ASSIGN_AUTH: u8 = 6;
    pub const REENTER: u8 = 7;
    pub const NATIVE_CLOSING: u8 = 8;
    pub const NATIVE_DIRECT: u8 = 9;
    pub const NOTHING: u8 = 10;
    pub const ALLOCATE_AUTH: u8 = 11;
    pub const FUND_AUTH: u8 = 12;
    pub const CLOSE_WSOL: u8 = 13;
    pub const PAY_2022: u8 = 14;
}
